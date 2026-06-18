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
    registrar_log("[GUARDIAN-P2P] Inicializando Agente Nativo Comercial...");

    // ── 1. Autenticación ───────────────────────────────────────────────────────
    let token = match obtener_token_seguridad().await {
        Ok(t) => { registrar_log("[AUTH] Token obtenido correctamente."); t }
        Err(e) => {
            registrar_log(&format!("[ERROR] Fallo crítico de autenticación: {}", e));
            return Ok(());
        }
    };

    // ── 2. Capturador de pantalla — ANTES del WebSocket para detectar fallos pronto
    //    Se mueve aquí para que el error de scrap sea visible en el log y no cuelgue
    //    silenciosamente más adelante tras el handshake SDP.
    registrar_log("[SCRAP] Inicializando capturador de pantalla...");
    let display = scrap::Display::primary().expect("[ERROR] Fallo al mapear pantalla primaria");
    let screen_width  = display.width()  as f64;
    let screen_height = display.height() as f64;
    // Capturer se construye en el spawn bloqueante para no congelar el runtime de tokio
    let (tx_frame, mut rx_frame) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let sw = screen_width  as u32;
    let sh = screen_height as u32;
    std::thread::spawn(move || {
        let mut capturador = match scrap::Capturer::new(display) {
            Ok(c) => c,
            Err(e) => { eprintln!("[ERROR-SCRAP] No se pudo crear Capturer: {}", e); return; }
        };
        loop {
            match capturador.frame() {
                Ok(sct_img) => {
                    let width  = sw as usize;
                    let height = sh as usize;
                    let bpp    = 4;
                    let stride = sct_img.len() / height;
                    let mut buffer_rgb = Vec::with_capacity(width * height * 3);
                    for y in 0..height {
                        let row_start = y * stride;
                        let row_data  = &sct_img[row_start..(row_start + width * bpp)];
                        for x in 0..width {
                            let p = x * bpp;
                            buffer_rgb.push(row_data[p + 2]); // R
                            buffer_rgb.push(row_data[p + 1]); // G
                            buffer_rgb.push(row_data[p + 0]); // B
                        }
                    }
                    // Escalar y comprimir en JPEG en el hilo bloqueante
                    if let Some(img_buf) = image::ImageBuffer::<image::Rgb<u8>, _>::from_raw(sw, sh, buffer_rgb) {
                        let escalada = image::DynamicImage::ImageRgb8(img_buf)
                            .resize(800, 450, image::imageops::FilterType::Triangle);
                        let mut jpeg_bytes = Vec::new();
                        let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 40);
                        if enc.encode_image(&escalada).is_ok() && jpeg_bytes.len() < 65_535 {
                            let _ = tx_frame.blocking_send(jpeg_bytes);
                        }
                    }
                    std::thread::sleep(Duration::from_millis(60));
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    });
    registrar_log("[SCRAP] Hilo de captura lanzado.");

    // ── 3. WebRTC ─────────────────────────────────────────────────────────────
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

    let (ws_sender, mut ws_receiver) = ws_stream.split();
    let ws_sender_mutex = Arc::new(Mutex::new(ws_sender));

    // ── 5. Canal de comandos de periférico ────────────────────────────────────
    let (tx_comando, mut rx_comando) = tokio::sync::mpsc::channel::<RemoteControlCommand>(100);

    // ── 6. DataChannel holder — se extrae el Arc antes de entrar al loop ──────
    //    FIX Bug 3: usamos un canal para pasar el DataChannel en lugar de un
    //    Mutex que se mantiene bloqueado durante el await de dc.send().
    let (tx_dc, mut rx_dc) = tokio::sync::mpsc::channel::<Arc<RTCDataChannel>>(1);
    let tx_comando_dc = tx_comando.clone();

    peer_connection.on_data_channel(Box::new(move |d| {
        let tx_cmd = tx_comando_dc.clone();
        let tx_dc  = tx_dc.clone();
        Box::pin(async move {
            if d.label() == "video_stream" {
                registrar_log("[DC] DataChannel 'video_stream' recibido.");
                d.on_message(Box::new(move |msg| {
                    let tx = tx_cmd.clone();
                    Box::pin(async move {
                        if let Ok(text) = std::str::from_utf8(&msg.data) {
                            if let Ok(cmd) = serde_json::from_str::<RemoteControlCommand>(text) {
                                let _ = tx.send(cmd).await;
                            }
                        }
                    })
                }));
                let _ = tx_dc.send(d).await;
            }
        })
    }));

    // ── 7. Señalización WebSocket en spawn separado ───────────────────────────
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

    // ── 8. ICE candidates salientes ───────────────────────────────────────────
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

    // ── 9. Esperar conexión P2P — con timeout para no colgar eternamente ──────
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
            registrar_log("[TIMEOUT] Sin conexión P2P en 120s. Comprueba que el visor está activo.");
            return Ok(());
        }
    }

    // ── 10. Esperar DataChannel y entrar en el loop principal ─────────────────
    let mut enigo = Enigo::new();
    let dc = match tokio::time::timeout(Duration::from_secs(10), rx_dc.recv()).await {
        Ok(Some(d)) => { registrar_log("[DC] DataChannel listo. Iniciando captura."); d }
        _ => { registrar_log("[ERROR] No se recibió el DataChannel a tiempo."); return Ok(()); }
    };

    loop {
        // Procesar comandos de periférico recibidos
        while let Ok(comando) = rx_comando.try_recv() {
            ejecutar_comando_periferico(comando, &mut enigo, screen_width, screen_height);
        }

        // Enviar frame si el canal está abierto y sin backpressure
        if dc.ready_state() == RTCDataChannelState::Open && dc.buffered_amount().await == 0 {
            if let Ok(jpeg_bytes) = rx_frame.try_recv() {
                let data_binaria = Bytes::from(jpeg_bytes);
                let _ = dc.send(&data_binaria).await;
            }
        }

        tokio::time::sleep(Duration::from_millis(16)).await;
    }
}