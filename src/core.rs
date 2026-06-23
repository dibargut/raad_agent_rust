// src/core.rs
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use tokio::net::UdpSocket;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use enigo::{Direction, Enigo, Keyboard, Mouse, Settings};
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::{rtp_codec::RTCRtpCodecCapability, rtp_transceiver_direction::RTCRtpTransceiverDirection};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

use crate::gui::AppState;
use crate::input::mapear_tecla;

#[derive(Serialize, Deserialize, Clone)]
struct CommandPayload {
    event: String,
    #[serde(default)] x_píxel: f64,
    #[serde(default)] y_píxel: f64,
    #[serde(default)] w_nativa: f64,
    #[serde(default)] h_nativa: f64,
    #[serde(default)] button: String,
    #[serde(default)] key: String,
    #[serde(default)] delta_x: i32,
    #[serde(default)] delta_y: i32,
}

#[derive(Serialize, Deserialize, Clone)]
struct SdpPayload { #[serde(rename = "type")] sdp_type: String, sdp: String }

#[derive(Serialize, Deserialize, Clone)]
struct SignalingMessage {
    #[serde(skip_serializing_if = "Option::is_none")] sdp: Option<SdpPayload>,
    #[serde(skip_serializing_if = "Option::is_none")] ice: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct AuthResponse { access_token: String }

pub async fn ejecutar_core_agente(id_pantalla: String, state: Arc<Mutex<AppState>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let backend_host = "192.168.1.135:8080";
    let password = "TuContrasenaSeguraAqui";
    let session_uuid = "test-session-123";

    let log = |msg: &str, s: &Arc<Mutex<AppState>>| { s.lock().unwrap().logs.push(msg.to_string()); };

    log("[AGENTE] Solicitando token de acceso via HTTP...", &state);
    let client = reqwest::Client::new();
    let auth_res = client
        .post(format!("http://{}/api/remote/auth/login", backend_host))
        .json(&serde_json::json!({ "password": password }))
        .send().await?;

    if !auth_res.status().is_success() { return Ok(()); }
    let auth_data: AuthResponse = auth_res.json().await?;
    let token = auth_data.access_token;

    let mut m = MediaEngine::default();
    m.register_default_codecs()?;
    let api = APIBuilder::new().with_media_engine(m).build();
    let pc = Arc::new(api.new_peer_connection(RTCConfiguration::default()).await?);

    let video_track = Arc::new(TrackLocalStaticRTP::new(
        RTCRtpCodecCapability { mime_type: MIME_TYPE_H264.to_string(), ..Default::default() },
        "video".to_string(), "stream".to_string(),
    ));

    pc.add_transceiver_from_track(
        Arc::clone(&video_track) as Arc<dyn TrackLocal + Send + Sync>,
        Some(webrtc::rtp_transceiver::RTCRtpTransceiverInit { direction: RTCRtpTransceiverDirection::Sendonly, send_encodings: vec![] }),
    ).await?;

    let (tx_control, mut rx_control) = tokio::sync::mpsc::channel::<CommandPayload>(100);
    let tx_on_data_channel = tx_control.clone();
    let state_dc = Arc::clone(&state);
    
    pc.on_data_channel(Box::new(move |d| {
        state_dc.lock().unwrap().logs.push(format!("[AGENTE] Canal Abierto por el Visor: {}", d.label()));
        let tx_clone = tx_on_data_channel.clone();
        Box::pin(async move {
            d.on_message(Box::new(move |msg| {
                if let Ok(text) = std::str::from_utf8(&msg.data) {
                    if let Ok(cmd) = serde_json::from_str::<CommandPayload>(text) {
                        let tx = tx_clone.clone();
                        tokio::spawn(async move { let _ = tx.send(cmd).await; });
                    }
                }
                Box::pin(async {})
            }));
        })
    }));

    let data_channel = pc.create_data_channel("control", None).await?;
    let tx_channel_clone = tx_control.clone();
    data_channel.on_message(Box::new(move |msg| {
        if let Ok(text) = std::str::from_utf8(&msg.data) {
            if let Ok(cmd) = serde_json::from_str::<CommandPayload>(text) {
                let tx = tx_channel_clone.clone();
                tokio::spawn(async move { let _ = tx.send(cmd).await; });
            }
        }
        Box::pin(async {})
    }));

    std::thread::spawn(move || {
        let mut enigo = Enigo::new(&Settings::default()).unwrap();
        let (w, h) = enigo.main_display().unwrap_or((1920, 1080));
        while let Some(cmd) = rx_control.blocking_recv() {
            match cmd.event.as_str() {
                "mouse_move" => {
                    if cmd.w_nativa > 0.0 && cmd.h_nativa > 0.0 {
                        let real_x = (cmd.x_píxel / cmd.w_nativa) * w as f64;
                        let real_y = (cmd.y_píxel / cmd.h_nativa) * h as f64;
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
                    if cmd.delta_y != 0 { let _ = enigo.scroll(cmd.delta_y, enigo::Axis::Vertical); }
                    if cmd.delta_x != 0 { let _ = enigo.scroll(cmd.delta_x, enigo::Axis::Horizontal); }
                }
                "key_down" => {
                    if let Some(k) = mapear_tecla(&cmd.key) {
                        match k {
                            enigo::Key::Unicode(c) => {
                                let mut buffer = [0; 4];
                                let str_char = c.encode_utf8(&mut buffer);
                                let _ = enigo.text(str_char);
                            }
                            tecla_especial => {
                                let _ = enigo.key(tecla_especial, Direction::Press);
                            }
                        }
                    }
                }
                "key_up" => {
                    if let Some(k) = mapear_tecla(&cmd.key) {
                        match k {
                            enigo::Key::Unicode(_) => {}
                            tecla_especial => {
                                let _ = enigo.key(tecla_especial, Direction::Release);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    });

    let state_wrtc = Arc::clone(&state);
    pc.on_peer_connection_state_change(Box::new(move |state| {
        state_wrtc.lock().unwrap().logs.push(format!("[WEBRTC] Estado: {}", state));
        Box::pin(async {})
    }));

    let ws_url = format!("ws://{}/api/remote/signaling/{}/agente?token={}", backend_host, session_uuid, token);
    let (ws_stream, _) = connect_async(ws_url).await?;
    let (ws_tx, mut ws_rx) = ws_stream.split();
    let ws_tx_clone = Arc::new(tokio::sync::Mutex::new(ws_tx));
    let ws_tx_ice = Arc::clone(&ws_tx_clone);
    
    pc.on_ice_candidate(Box::new(move |candidate| {
        if let Some(cand) = candidate {
            let msg_ice = SignalingMessage { sdp: None, ice: Some(serde_json::to_value(cand.to_json().unwrap()).unwrap()) };
            let json_string = serde_json::to_string(&msg_ice).unwrap();
            let ws_tx_lock = Arc::clone(&ws_tx_ice);
            tokio::spawn(async move { let _ = ws_tx_lock.lock().await.send(Message::Text(json_string.into())).await; });
        }
        Box::pin(async {})
    }));

    sleep(Duration::from_millis(1500)).await;
    let offer = pc.create_offer(None).await?;
    pc.set_local_description(offer.clone()).await?;

    let mensaje_oferta = SignalingMessage { sdp: Some(SdpPayload { sdp_type: "offer".to_string(), sdp: offer.sdp }), ice: None };
    ws_tx_clone.lock().await.send(Message::Text(serde_json::to_string(&mensaje_oferta)?.into())).await?;
    log("[AGENTE] Oferta SDP enviada al visor.", &state);

    let id_pantalla_ffmpeg = id_pantalla.clone();
    let track_clone = Arc::clone(&video_track);
    
    tokio::spawn(async move {
        let listener = UdpSocket::bind("127.0.0.1:5004").await.unwrap();
        
        // 🎯 Autodescubrimiento dinámico del índice de pantalla real en macOS
        #[cfg(target_os = "macos")]
        let indice_pantalla = {
            let output = std::process::Command::new("ffmpeg")
                .args(&["-f", "avfoundation", "-list_devices", "true", "-i", ""])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stderr).into_owned())
                .unwrap_or_default();

            let mut encontrado = None;
            for line in output.lines() {
                if line.contains("Capture screen") {
                    let fragmento = &line[..line.find("Capture screen").unwrap_or(line.len())];
                    if let Some(pos_corchete) = fragmento.rfind('[') {
                        if let Some(pos_cierre) = fragmento[pos_corchete..].find(']') {
                            let sub = &fragmento[pos_corchete + 1..pos_corchete + pos_cierre];
                            if sub.chars().all(|c| c.is_ascii_digit()) {
                                encontrado = Some(sub.to_string());
                                break;
                            }
                        }
                    }
                }
            }
            encontrado.unwrap_or_else(|| "1".to_string())
        };

        #[cfg(not(target_os = "macos"))]
        let indice_pantalla = id_pantalla_ffmpeg.trim().to_string();

        let avf_index = format!("{}:", indice_pantalla);
        println!("[AGENTE] Índice de captura asignado dinámicamente: '{}'", avf_index);

        #[cfg(target_os = "macos")]
        let ffmpeg_args = vec![
            "-f", "avfoundation",
            "-capture_cursor", "1",
            "-pixel_format", "nv12",
            "-i", &avf_index, 
            "-r", "30",
            "-vf", "scale=1280:-2:flags=lanczos,format=yuv420p",
            "-vcodec", "h264_videotoolbox", 
            "-realtime", "1",
            "-bf", "0",
            "-profile:v", "baseline",
            "-prio_speed", "1",
            "-b:v", "2000k",
            "-maxrate", "2500k",
            "-bufsize", "4000k",
            "-g", "30",
            "-bsf:v", "dump_extra", 
            "-f", "rtp",
            "-payload_type", "96", 
            "rtp://127.0.0.1:5004?pkt_size=1200&buffer_size=10485760"
        ];

        #[cfg(target_os = "linux")]
        let ffmpeg_args = vec![
            "-f", "x11grab", 
            "-video_size", "1280x720", 
            "-i", &indice_pantalla,
            "-r", "30", 
            "-c:v", "h264_v4l2m2m", 
            "-b:v", "2M", 
            "-pix_fmt", "yuv420p", 
            "-f", "rtp", 
            "-payload_type", "96", 
            "rtp://127.0.0.1:5004?pkt_size=1200"
        ];

        let mut child = Command::new("ffmpeg")
            .args(&ffmpeg_args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .unwrap();
            
        loop {
            let mut inbound_buffer = vec![0u8; 2048];
            match listener.recv_from(&mut inbound_buffer).await {
                Ok((n, _)) => { 
                    if n > 0 {
                        let packet_data = inbound_buffer[..n].to_vec();
                        let track_resilient = Arc::clone(&track_clone);
                        if track_resilient.write(&packet_data).await.is_err() { 
                            break; 
                        } 
                    }
                }
                Err(_) => break,
            }
        }
        let _ = child.kill().await;
    });

    let pc_clone = Arc::clone(&pc);
    while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Text(text) = msg {
            if let Ok(payload) = serde_json::from_str::<SignalingMessage>(text.as_str()) {
                if let Some(sdp_data) = payload.sdp {
                    if sdp_data.sdp_type == "answer" {
                        let sdp_json_string = serde_json::json!({"type": "answer", "sdp": sdp_data.sdp}).to_string();
                        if let Ok(rd) = serde_json::from_str::<RTCSessionDescription>(&sdp_json_string) {
                            pc_clone.set_remote_description(rd).await?;
                            log("[AGENTE] Handshake completado con éxito.", &state);
                        }
                    }
                } else if let Some(ice_data) = payload.ice {
                    if let Ok(ice_init) = serde_json::from_value::<webrtc::ice_transport::ice_candidate::RTCIceCandidateInit>(ice_data) {
                        let _ = pc_clone.add_ice_candidate(ice_init).await;
                    }
                }
            }
        }
    }
    Ok(())
}