use std::fs::OpenOptions;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;
use chrono::Local;
use enigo::{Enigo, MouseControllable, KeyboardControllable, Key};
use futures_util::{StreamExt, SinkExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Mutex;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use url::Url;
use bytes::Bytes;

use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::data_channel::RTCDataChannel;
use webrtc::data_channel::data_channel_state::RTCDataChannelState;

const BACKEND_HOST: &str = "192.168.1.135";
const BACKEND_PORT: &str = "8080";
const ACCESS_PASSWORD: &str = "TuContrasenaSeguraAqui";
const SESSION_UUID: &str = "test-session-123";

#[derive(Deserialize)]
struct LoginResponse { access_token: String }

#[derive(Deserialize, Debug)]
struct RemoteControlCommand {
    event: String,
    #[serde(default)] x_píxel: f64,
    #[serde(default)] y_píxel: f64,
    #[serde(default)] w_nativa: f64,
    #[serde(default)] h_nativa: f64,
    #[serde(default)] button: String,
    #[serde(default)] key: String,
    #[serde(default)] deltaY: f64,
}

fn registrar_log(mensaje: &str) {
    let fecha = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let linea_log = format!("[{}] {}\n", fecha, mensaje);
    print!("{}", linea_log);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open("guardian.log") {
        let _ = file.write_all(linea_log.as_bytes());
    }
}

async fn obtener_token_seguridad() -> Result<String, Box<dyn std::error::Error>> {
    let url = format!("http://{}:{}/api/remote/auth/login", BACKEND_HOST, BACKEND_PORT);
    let client = reqwest::Client::new();
    let res = client.post(&url)
        .json(&json!({ "password": ACCESS_PASSWORD }))
        .timeout(Duration::from_secs(5))
        .send()
        .await?;
    if res.status().is_success() {
        let body: LoginResponse = res.json().await?;
        Ok(body.access_token)
    } else {
        Err("Credenciales SRA rechazadas".into())
    }
}

fn ejecutar_comando_periferico(comando: RemoteControlCommand, enigo: &mut Enigo, s_width: f64, s_height: f64) {
    let (target_x, target_y) = if comando.w_nativa > 0.0 && comando.h_nativa > 0.0 {
        let escala_x = s_width / comando.w_nativa;
        let escala_y = s_height / comando.h_nativa;
        ((comando.x_píxel * escala_x) as i32, (comando.y_píxel * escala_y) as i32)
    } else {
        (comando.x_píxel as i32, comando.y_píxel as i32)
    };
    match comando.event.as_str() {
        "mouse_move" | "mouse_drag" => { enigo.mouse_move_to(target_x, target_y); }
        "mouse_down" => {
            enigo.mouse_move_to(target_x, target_y);
            let btn = if comando.button == "right" { enigo::MouseButton::Right } else { enigo::MouseButton::Left };
            enigo.mouse_down(btn);
        }
        "mouse_up" => {
            let btn = if comando.button == "right" { enigo::MouseButton::Right } else { enigo::MouseButton::Left };
            enigo.mouse_up(btn);
        }
        "mouse_scroll" => { enigo.mouse_scroll_y(if comando.deltaY > 0.0 { -1 } else { 1 }); }
        "key_press" => {
            match comando.key.as_str() {
                "Enter"     => enigo.key_click(Key::Layout('\n')),
                "Backspace" => enigo.key_click(Key::Backspace),
                "Tab"       => enigo.key_click(Key::Tab),
                "Space" | " " => enigo.key_click(Key::Space),
                "ArrowUp"   => enigo.key_click(Key::UpArrow),
                "ArrowDown" => enigo.key_click(Key::DownArrow),
                "ArrowLeft" => enigo.key_click(Key::LeftArrow),
                "ArrowRight"=> enigo.key_click(Key::RightArrow),
                _ => { if comando.key.chars().count() == 1 { enigo.key_sequence(&comando.key); } }
            }
        }
        _ => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    registrar_log("[GUARDIAN-P2P] Inicializando Agente Multiplataforma Optimizada...");

    // ── 1. Inicialización de xcap Multiplataforma Nativo ────────────────────────
    registrar_log("[XCAP] Mapeando pantalla principal del sistema...");
    let monitor = xcap::Monitor::all()
        .expect("[ERROR] No se pudo acceder a la lista de pantallas")
        .into_iter()
        .next()
        .expect("[ERROR] Ningún monitor activo detectado.");

    let screen_width  = monitor.width().unwrap() as f64;
    let screen_height = monitor.height().unwrap() as f64;
    registrar_log(&format!("[XCAP] Monitor inicializado correctamente: {}x{}", screen_width, screen_height));

    // ── 2. Autenticación ───────────────────────────────────────────────────────
    let token = match obtener_token_seguridad().await {
        Ok(t) => { registrar_log("[AUTH] Token obtenido correctamente."); t }
        Err(e) => {
            registrar_log(&format!("[ERROR] Fallo crítico de autenticación: {}", e));
            return Ok(());
        }
    };

    // ── 3. WebRTC Configuración inicial ───────────────────────────────────────
    let m_engine = MediaEngine::default();
    let api = APIBuilder::new().with_media_engine(m_engine).build();
    let config = RTCConfiguration {
        ice_servers: vec![RTCIceServer {
            urls: vec!["stun:stun.l.google.com:19302".to_owned()],
            ..Default::default()
        }],
        ..Default::default()
    };
    let peer_connection = Arc::new(api.new_peer_connection(config).await?);
    registrar_log("[WEBRTC] PeerConnection creada.");

    // ── 4. WebSocket de señalización ──────────────────────────────────────────
    let ws_url = format!(
        "ws://{}:{}/api/remote/signaling/{}/agente?token={}",
        BACKEND_HOST, BACKEND_PORT, SESSION_UUID, token
    );
    registrar_log(&format!("[WS] Conectando a señalización: {}", ws_url));
    let (ws_stream, _) = connect_async(Url::parse(&ws_url)?)
        .await
        .map_err(|e| { registrar_log(&format!("[ERROR] Fallo WebSocket: {}", e)); e })?;
    registrar_log("[WS] Conectado al servidor de señalización.");

    let (ws_stream_sender, mut ws_receiver) = ws_stream.split();
    let ws_sender_mutex = Arc::new(Mutex::new(ws_stream_sender));

    // ── 5. Canales MPSC de comunicación asíncrona ─────────────────────────────
    let (tx_comando, mut rx_comando) = tokio::sync::mpsc::channel::<RemoteControlCommand>(100);
    let (tx_dc, mut rx_dc) = tokio::sync::mpsc::channel::<Arc<RTCDataChannel>>(1);
    // Canal táctico para recibir los frames JPEG desde el hilo de captura
    let (tx_video, mut rx_video) = tokio::sync::mpsc::channel::<Vec<u8>>(2);

    // Escuchar la inicialización del canal multimedia externo
    peer_connection.on_data_channel(Box::new(move |d| {
        let tx_dc = tx_dc.clone();
        Box::pin(async move {
            if d.label() == "video_stream" {
                registrar_log("[DC] DataChannel 'video_stream' detectado por WebRTC.");
                let _ = tx_dc.send(d).await;
            }
        })
    }));

    // ── 6. Manejo asíncrono de Señalización en Tokio Task ─────────────────────
    let pc_clone        = Arc::clone(&peer_connection);
    let ws_sender_clone = Arc::clone(&ws_sender_mutex);

    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_receiver.next().await {
            let text = match msg {
                Message::Text(t) => t,
                Message::Close(_) => { registrar_log("[WS] Señalización cerrada por el servidor."); break; }
                _ => continue,
            };
            let data = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match data["type"].as_str().unwrap_or("") {
                "offer" => {
                    registrar_log("[SDP] Offer recibida del visor.");
                    let sdp = match data["sdp"].as_str() {
                        Some(s) => s.to_string(),
                        None => { registrar_log("[ERROR] Offer sin campo sdp"); continue; }
                    };
                    let desc = match RTCSessionDescription::offer(sdp) {
                        Ok(d) => d,
                        Err(e) => { registrar_log(&format!("[ERROR] SDP inválida: {}", e)); continue; }
                    };
                    if pc_clone.set_remote_description(desc).await.is_err() {
                        registrar_log("[ERROR] set_remote_description falló"); continue;
                    }
                    match pc_clone.create_answer(None).await {
                        Ok(answer) => {
                            let _ = pc_clone.set_local_description(answer.clone()).await;
                            let payload = json!({ "type": "answer", "sdp": answer.sdp }).to_string();
                            let mut sender = ws_sender_clone.lock().await;
                            let _ = sender.send(Message::Text(payload)).await;
                            registrar_log("[SDP] Answer enviada al visor.");
                        }
                        Err(e) => registrar_log(&format!("[ERROR] create_answer: {}", e)),
                    }
                }
                "candidate" => {
                    if let Ok(cand) = serde_json::from_value::<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>(data["candidate"].clone()) {
                        let _ = pc_clone.add_ice_candidate(cand).await;
                    }
                }
                other => { registrar_log(&format!("[WS] Mensaje desconocido: {}", other)); }
            }
        }
    });

    // ── 7. ICE candidates salientes ───────────────────────────────────────────
    let ws_sender_ice = Arc::clone(&ws_sender_mutex);
    peer_connection.on_ice_candidate(Box::new(move |c| {
        let ws_sender_ice = Arc::clone(&ws_sender_ice);
        Box::pin(async move {
            if let Some(candidate) = c {
                if let Ok(ice_json) = candidate.to_json() {
                    let json_ice = json!({ "type": "candidate", "candidate": ice_json }).to_string();
                    let mut sender = ws_sender_ice.lock().await;
                    let _ = sender.send(Message::Text(json_ice)).await;
                }
            }
        })
    }));

    // ── 8. Esperar conexión P2P (Estado de red) ───────────────────────────────
    let (tx_active, mut rx_active) = tokio::sync::mpsc::channel::<bool>(1);
    peer_connection.on_peer_connection_state_change(Box::new(move |s| {
        registrar_log(&format!("[WEBRTC] Estado de conexión: {:?}", s));
        let tx = tx_active.clone();
        Box::pin(async move {
            if s == RTCPeerConnectionState::Connected {
                let _ = tx.send(true).await;
            }
        })
    }));

    registrar_log("[GUARDIAN] Esperando conexión P2P del visor (timeout 120s)...");
    tokio::select! {
        _ = rx_active.recv() => { registrar_log("[GUARDIAN] ¡Conexión P2P establecida!"); }
        _ = tokio::time::sleep(Duration::from_secs(120)) => {
            registrar_log("[TIMEOUT] Sin conexión P2P en 120s.");
            return Ok(());
        }
    }

    // ── 9. Acoplar el DataChannel activo ──────────────────────────────────────
    let mut enigo = Enigo::new();
    let dc = match tokio::time::timeout(Duration::from_secs(10), rx_dc.recv()).await {
        Ok(Some(d)) => { registrar_log("[DC] DataChannel recibido en el hilo principal."); d }
        _ => { registrar_log("[ERROR] No se recibió el DataChannel a tiempo."); return Ok(()); }
    };

    let tx_comando_loop = tx_comando.clone();
    dc.on_message(Box::new(move |msg| {
        let tx = tx_comando_loop.clone();
        Box::pin(async move {
            if let Ok(text) = std::str::from_utf8(&msg.data) {
                if let Ok(cmd) = serde_json::from_str::<RemoteControlCommand>(text) {
                    let _ = tx.try_send(cmd);
                }
            }
        })
    }));

    tokio::time::sleep(Duration::from_millis(200)).await;

    if dc.ready_state() == RTCDataChannelState::Open {
        let _ = dc.send(&Bytes::from_static(b"PING")).await;
        registrar_log("[DC] Enviado PING de diagnóstico inicial al visor.");
    }

    // ── 10. GENERAR TRABAJO INDEPENDIENTE PARA CAPTURA DE PANTALLA (Multi-threading) ──
    registrar_log("[ENGINE] Lanzando orquestador gráfico en hilo paralelo...");
    tokio::task::spawn_blocking(move || {
        loop {
            // Captura directa en formato plano multiplataforma
            if let Ok(imagen_xcap) = monitor.capture_image() {
                let ancho_real = imagen_xcap.width();
                let alto_real = imagen_xcap.height();
                
                // Convertimos la imagen plana de xcap en un contenedor manejable por image crate
                let raw_pixels = imagen_xcap.into_raw();
                
                if let Some(img_buf) = image::ImageBuffer::<image::Rgba<u8>, _>::from_raw(
                    ancho_real, 
                    alto_real, 
                    raw_pixels
                ) {
                    // Reescalado inteligente de alta velocidad libre de lag por hardware
                    let escalada = image::DynamicImage::ImageRgba8(img_buf)
                        .thumbnail(640, 360);
                    
                    let mut jpeg_bytes = Vec::new();
                    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 40); 
                    
                    if enc.encode_image(&escalada).is_ok() {
                        // Enviamos los bytes listos al bucle WebRTC sin saturar la memoria (drop reactivo)
                        let _ = tx_video.try_send(jpeg_bytes);
                    }
                }
            }
            // Mantenemos un muestreo uniforme estable para aliviar la CPU (~30 FPS continuos)
            std::thread::sleep(Duration::from_millis(33));
        }
    });

    // ── 11. BUCLE ASÍNCRONO DE ALTA FLUIDEZ (MAIN THREAD LIBERADO) ────────────────────
    let mut contador_frames = 0;

    loop {
        // A. Despachar comandos de periféricos entrantes instantáneamente
        while let Ok(comando) = rx_comando.try_recv() {
            registrar_log(&format!("[RATON-1:1] Ejecutando evento: {}", comando.event));
            ejecutar_comando_periferico(comando, &mut enigo, screen_width, screen_height);
        }

        // B. Transmisión asíncrona de vídeo no bloqueante
        if dc.ready_state() == RTCDataChannelState::Open {
            // Si el hilo paralelo tiene un frame listo, lo inyectamos de inmediato a WebRTC
            if let Ok(jpeg_bytes) = rx_video.try_recv() {
                let len_bytes = jpeg_bytes.len();
                if len_bytes < 65535 {
                    let data_binaria = Bytes::from(jpeg_bytes);
                    if dc.send(&data_binaria).await.is_ok() {
                        contador_frames += 1;
                        if contador_frames % 30 == 0 {
                            registrar_log(&format!("[TELEMETRIA] Canal libre: {} frames enviados en paralelo. Tamaño: {} bytes", contador_frames, len_bytes));
                        }
                    }
                }
            }
        } else {
            if dc.ready_state() == RTCDataChannelState::Closed || dc.ready_state() == RTCDataChannelState::Closing {
                registrar_log("[WEBRTC] El DataChannel se ha cerrado de forma externa. Saliendo.");
                break;
            }
        }

        // Latencia mínima del bucle central: El ratón responderá de forma asíncrona a 1ms
        tokio::time::sleep(Duration::from_millis(1)).await;
    }

    Ok(())
}