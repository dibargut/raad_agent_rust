use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

// API Media e hilos principales
use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;

// enigo para control de gestos y mouse
use enigo::{Direction, Enigo, Key, Keyboard, Mouse, Settings};

// Configuración y estados de Peer Connection
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

// Direcciones y Códecs del Transceiver
use webrtc::rtp_transceiver::{
    rtp_codec::RTCRtpCodecCapability,
    rtp_transceiver_direction::RTCRtpTransceiverDirection,
};

// Tracks Locales
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

#[derive(Serialize, Deserialize, Clone)]
struct CommandPayload {
    event: String,
    // Ratón y Clics
    #[serde(default)]
    x_píxel: f64,
    #[serde(default)]
    y_píxel: f64,
    #[serde(default)]
    w_nativa: f64,
    #[serde(default)]
    h_nativa: f64,
    #[serde(default)]
    button: String,
    // Teclado
    #[serde(default)]
    key: String,
    // Scroll
    #[serde(default)]
    delta_x: i32,
    #[serde(default)]
    delta_y: i32,
}

#[derive(Serialize, Deserialize, Clone)]
struct SdpPayload {
    #[serde(rename = "type")]
    sdp_type: String,
    sdp: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct SignalingMessage {
    #[serde(skip_serializing_if = "Option::is_none")]
    sdp: Option<SdpPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ice: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AuthResponse {
    access_token: String,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backend_host = "192.168.1.135:8080";
    let password = "TuContrasenaSeguraAqui";
    let session_uuid = "test-session-123";

    println!("[AGENTE] Solicitando token de acceso via HTTP...");
    let client = reqwest::Client::new();
    let auth_res = client
        .post(format!("http://{}/api/remote/auth/login", backend_host))
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await?;

    if !auth_res.status().is_success() {
        eprintln!("[AGENTE-ERROR] Falló la autenticación HTTP.");
        return Ok(());
    }

    let auth_data: AuthResponse = auth_res.json().await?;
    let token = auth_data.access_token;

    let mut m = MediaEngine::default();
    m.register_default_codecs()?;
    let api = APIBuilder::new().with_media_engine(m).build();

    let config = RTCConfiguration::default();
    let pc = Arc::new(api.new_peer_connection(config).await?);

    let video_track = Arc::new(TrackLocalStaticRTP::new(
        RTCRtpCodecCapability {
            mime_type: MIME_TYPE_H264.to_string(),
            ..Default::default()
        },
        "video".to_string(),
        "stream".to_string(),
    ));

    pc.add_transceiver_from_track(
        Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>,
        Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Sendonly,
            send_encodings: vec![],
        }),
    )
    .await?;

    pc.on_data_channel(Box::new(|d| {
        println!("[AGENTE] Canal de control abierto por el Visor via WebRTC DataChannel: {}", d.label());
        Box::pin(async move {
            d.on_message(Box::new(|msg| {
                if let Ok(text) = std::str::from_utf8(&msg.data) {
                    println!("[COMMAND-DC] Comando por canal seguro: {}", text);
                }
                Box::pin(async {})
            }));
        })
    }));

    pc.on_peer_connection_state_change(Box::new(|state| {
        println!("[AGENTE-WEBRTC] Estado de la conexión: {}", state);
        if state == RTCPeerConnectionState::Connected {
            println!("[AGENTE-WEBRTC] ✅ ¡Túnel WebRTC establecido con éxito!");
        }
        Box::pin(async {})
    }));

    let ws_url = format!("ws://{}/api/remote/signaling/{}/agente?token={}", backend_host, session_uuid, token);
    println!("[AGENTE] Conectando al WebSocket de señalización...");
    let (ws_stream, _) = connect_async(ws_url).await?;
    let (mut ws_tx, mut ws_rx) = ws_stream.split();

    // Capturar candidatos ICE locales generados por este Mac y enviárselos al Visor
    let ws_tx_clone = Arc::new(tokio::sync::Mutex::new(ws_tx));
    let ws_tx_ice = Arc::clone(&ws_tx_clone);
    pc.on_ice_candidate(Box::new(move |candidate| {
        if let Some(cand) = candidate {
            let json_cand = cand.to_json().unwrap();
            let msg_ice = SignalingMessage {
                sdp: None,
                ice: Some(serde_json::to_value(json_cand).unwrap()),
            };
            let json_string = serde_json::to_string(&msg_ice).unwrap();
            let ws_tx_lock = Arc::clone(&ws_tx_ice);
            tokio::spawn(async move {
                let mut guard = ws_tx_lock.lock().await;
                let _ = guard.send(Message::Text(json_string.into())).await;
            });
        }
        Box::pin(async {})
    }));

    println!("[AGENTE] Conectado. Esperando estabilización ICE... ");
    sleep(Duration::from_millis(1500)).await;

    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer.clone()).await?;

    let mensaje_oferta = SignalingMessage {
        sdp: Some(SdpPayload {
            sdp_type: "offer".to_string(),
            sdp: offer.sdp,
        }),
        ice: None,
    };

    let json_oferta = serde_json::to_string(&mensaje_oferta)?;
    {
        let mut guard = ws_tx_clone.lock().await;
        guard.send(Message::Text(json_oferta.into())).await?;
    }
    println!("[AGENTE] Oferta SDP enviada al visor.");

    // Pipeline FFmpeg -> UDP -> WebRTC
    let track_clone = Arc::clone(&video_track);
    tokio::spawn(async move {
        let listener = match UdpSocket::bind("127.0.0.1:5004").await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[AGENTE-VIDEO-ERROR] No se pudo bindiar el puerto UDP local 5004: {}", e);
                return;
            }
        };
        
        println!("[AGENTE-VIDEO] Lanzando subproceso FFmpeg empaquetando en formato RTP nativo...");

        let mut child = match Command::new("ffmpeg")
            .args(&[
                "-f", "avfoundation",
                "-capture_cursor", "1",
                "-pixel_format", "nv12",
                "-i", "1",
                "-r", "30",
                "-vf", "scale=1280:-1",      
                "-vcodec", "h264_videotoolbox", 
                "-realtime", "1",
                "-bf", "0",                  
                "-prio_speed", "1",
                "-q:v", "55",                
                "-g", "30",                  
                "-f", "rtp",
                "-payload_type", "96", 
                "rtp://127.0.0.1:5004?pkt_size=1200&buffer_size=10485760"
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()) 
            .spawn() 
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[AGENTE-VIDEO-ERROR] Error ejecutando FFmpeg: {}", e);
                return;
            }
        };

        let mut inbound_buffer = vec![0u8; 2048];

        loop {
            match listener.recv_from(&mut inbound_buffer).await {
                Ok((n, _)) => {
                    if let Err(e) = track_clone.write(&inbound_buffer[..n]).await {
                        eprintln!("[AGENTE-VIDEO-ERROR] Error al inyectar paquete RTP en WebRTC: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[AGENTE-VIDEO-ERROR] Error leyendo del socket UDP: {}", e);
                    break;
                }
            }
        }

        let _ = child.kill().await;
    });

    // Inicialización del motor Enigo para macOS
    let mut enigo = Enigo::new(&Settings::default()).unwrap();
    
    // Dimensiones lógicas reales de tu monitor (Cámbialas según tu pantalla si es necesario)
    let pantalla_ancho_real = 1440.0;
    let pantalla_alto_real = 900.0;

    // Escucha de mensajes del WebSocket
    let pc_clone = Arc::clone(&pc);
    while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Text(text) = msg {
            let text_str = text.as_str();
            
            if let Ok(payload) = serde_json::from_str::<SignalingMessage>(text_str) {
                // 1. Procesar Respuesta SDP (Answer)
                if let Some(sdp_data) = payload.sdp {
                    if sdp_data.sdp_type == "answer" {
                        println!("[AGENTE] Recibida respuesta 'answer' del Visor. Aplicando...");
                        let sdp_json_string = serde_json::json!({
                            "type": "answer",
                            "sdp": sdp_data.sdp
                        }).to_string();

                        if let Ok(rd) = serde_json::from_str::<RTCSessionDescription>(&sdp_json_string) {
                            pc_clone.set_remote_description(rd).await?;
                            println!("[AGENTE] Handshake de señalización completado.");
                        }
                    }
                }
                // 2. Procesar Candidatos ICE remotos
                else if let Some(ice_data) = payload.ice {
                    if let Ok(ice_init) = serde_json::from_value::<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>(ice_data) {
                        if let Err(e) = pc_clone.add_ice_candidate(ice_init).await {
                            eprintln!("[AGENTE-ICE-WARN] Error agregando candidato ICE del visor: {}", e);
                        }
                    } else {
                        eprintln!("[AGENTE-ICE-ERROR] Falló al deserializar el candidato ICE recibido.");
                    }
                }
            } 
            // 🔥 CONTROL INTERACTIVO: Decodificar y ejecutar acciones físicas de perifericos
            else if let Ok(cmd) = serde_json::from_str::<CommandPayload>(text_str) {
                match cmd.event.as_str() {
                    "mouse_move" => {
                        if cmd.w_nativa > 0.0 && cmd.h_nativa > 0.0 {
                            let real_x = (cmd.x_píxel / cmd.w_nativa) * pantalla_ancho_real;
                            let real_y = (cmd.y_píxel / cmd.h_nativa) * pantalla_alto_real;
                            let _ = enigo.move_mouse(real_x as i32, real_y as i32, enigo::Coordinate::Abs);
                        }
                    }
                    "mouse_down" => {
                        let boton = if cmd.button == "right" { enigo::Button::Right } else { enigo::Button::Left };
                        let _ = enigo.button(boton, Direction::Press);
                    }
                    "mouse_up" => {
                        let boton = if cmd.button == "right" { enigo::Button::Right } else { enigo::Button::Left };
                        let _ = enigo.button(boton, Direction::Release);
                    }
                    "scroll" => {
                        if cmd.delta_y != 0 {
                            let _ = enigo.scroll(cmd.delta_y, enigo::Axis::Vertical);
                        }
                        if cmd.delta_x != 0 {
                            let _ = enigo.scroll(cmd.delta_x, enigo::Axis::Horizontal);
                        }
                    }
                    "key_down" => {
                        if let Some(k) = mapear_tecla(&cmd.key) {
                            let _ = enigo.key(k, Direction::Press);
                        }
                    }
                    "key_up" => {
                        if let Some(k) = mapear_tecla(&cmd.key) {
                            let _ = enigo.key(k, Direction::Release);
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

// Mapeador auxiliar de cadenas de texto Web (JavaScript) a estructuras nativas de Enigo
fn mapear_tecla(key_str: &str) -> Option<Key> {
    match key_str {
        "Enter" => Some(Key::Return),
        "Backspace" => Some(Key::Backspace),
        "Tab" => Some(Key::Tab),
        "Escape" => Some(Key::Escape),
        "Space" | " " => Some(Key::Space),
        "ArrowUp" => Some(Key::UpArrow),
        "ArrowDown" => Some(Key::DownArrow),
        "ArrowLeft" => Some(Key::LeftArrow),
        "ArrowRight" => Some(Key::RightArrow),
        "Meta" | "Command" => Some(Key::Meta),
        "Shift" => Some(Key::Shift),
        "Control" => Some(Key::Control),
        "Alt" => Some(Key::Alt),
        s if s.len() == 1 => {
            if let Some(c) = s.chars().next() {
                Some(Key::Unicode(c))
            } else {
                None
            }
        }
        _ => None,
    }
}