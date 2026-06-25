// src/core.rs
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UdpSocket;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use enigo::{Direction, Enigo, Keyboard, Mouse, Settings};
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::{rtp_codec::RTCRtpCodecCapability, rtp_transceiver_direction::RTCRtpTransceiverDirection};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

use crate::gui::{AppState, TipoConexion};
use crate::input::mapear_tecla;

static SELECCION_MONITOR_SECUNDARIO: AtomicBool = AtomicBool::new(false);
static REINICIAR_FFMPEG_TX: Mutex<Option<tokio::sync::mpsc::Sender<()>>> = Mutex::new(None);

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

#[derive(Deserialize)]
struct ControlSignal { action: String, session_uuid: String }

// =======================================================================
// 🖥️ DETECCIÓN ADAPTATIVA DE PANTALLAS EN MACOS (COMPATIBLE CON DISPLAYLINK)
// =======================================================================

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct PantallaReal {
    pub avf_index: String,
    pub etiqueta: String,
}

#[cfg(target_os = "macos")]
pub async fn obtener_pantallas_reales_macos() -> Vec<PantallaReal> {
    // Consultamos directamente a FFmpeg los dispositivos AVFoundation de vídeo
    let output = Command::new("ffmpeg")
        .args(&["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .await;

    let mut pantallas_detectadas = Vec::new();
    let palabras_camara = ["facetime", "webcam", "camera", "iphone", "ipad", "continuity"];

    if let Ok(out) = output {
        let stderr_txt = String::from_utf8_lossy(&out.stderr);
        
        // Aislamos el bloque de vídeo ignorando los dispositivos de audio
        let bloque_video = stderr_txt.split("AVFoundation audio devices").next().unwrap_or(&stderr_txt);

        for line in bloque_video.lines() {
            let linea_lower = line.to_lowercase();
            
            // Buscamos flujos que capturen pantallas (incluyendo pantallas DisplayLink/Virtuales)
            if linea_lower.contains("capture screen") || linea_lower.contains("capture_screen") {
                // Filtro estricto anti-cámara para evitar falsos positivos
                if palabras_camara.iter().any(|kw| linea_lower.contains(kw)) {
                    continue;
                }

                if let Some(pos_dispositivo) = line.find(']') {
                    let resto_linea = &line[pos_dispositivo + 1..];
                    if let (Some(start), Some(end)) = (resto_linea.find('['), resto_linea.find(']')) {
                        let idx = resto_linea[start + 1..end].trim().to_string();
                        if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) {
                            // Evitamos duplicados
                            if !pantallas_detectadas.iter().any(|p: &PantallaReal| p.avf_index == idx) {
                                pantallas_detectadas.push(PantallaReal {
                                    avf_index: idx,
                                    etiqueta: String::new(),
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Fallback: Si FFmpeg no devolvió nada por un cambio de entorno, usamos el conteo físico de AppleScript
    if pantallas_detectadas.is_empty() {
        let output_pantallas = Command::new("osascript")
            .args(&["-e", "tell application \"Finder\" to get count of screens"])
            .output()
            .await;

        let mut conteo = 1;
        if let Ok(out) = output_pantallas {
            if let Ok(num) = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>() {
                conteo = num;
            }
        }
        for i in 0..conteo {
            pantallas_detectadas.push(PantallaReal {
                avf_index: i.to_string(),
                etiqueta: String::new(),
            });
        }
    }

    // Formateamos las etiquetas legibles que se pintarán en el dropdown de la GUI
    for (i, pantalla) in pantallas_detectadas.iter_mut().enumerate() {
        pantalla.etiqueta = if i == 0 {
            format!("Monitor Principal (Pantalla 1 (Laptop)) [AVF idx {}]", pantalla.avf_index)
        } else {
            format!("Monitor Externo (Pantalla {}) [AVF idx {}]", i + 1, pantalla.avf_index)
        };
    }

    pantallas_detectadas
}

#[cfg(target_os = "macos")]
pub async fn obtener_indices_pantalla_macos() -> Vec<String> {
    obtener_pantallas_reales_macos()
        .await
        .into_iter()
        .map(|p| p.avf_index)
        .collect()
}

pub fn conmutar_pantalla_caliente(hacia_secundaria: bool) {
    SELECCION_MONITOR_SECUNDARIO.store(hacia_secundaria, Ordering::SeqCst);
    if let Some(tx) = &*REINICIAR_FFMPEG_TX.lock().unwrap() {
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            let _ = tx_clone.send(()).await;
        });
    }
}

// =======================================================================
// 🔌 FUNCIÓN PRINCIPAL: TÚNEL DE CONTROL PERSISTENTE (IDLE)
// =======================================================================
pub async fn ejecutar_core_agente(id_pantalla: String, state: Arc<Mutex<AppState>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let backend_host = "192.168.1.135:8080";
    let password = "TuContrasenaSeguraAqui";
    let session_uuid = "test-session-123";

    let log = |msg: &str, s: &Arc<Mutex<AppState>>| { s.lock().unwrap().logs.push(msg.to_string()); };

    loop {
        log("[TÚNEL] Solicitando token de acceso via HTTP...", &state);
        let client = reqwest::Client::new();
        let auth_res_result = client
            .post(format!("http://{}/api/remote/auth/login", backend_host))
            .json(&serde_json::json!({ "password": password }))
            .send().await;

        let auth_res = match auth_res_result {
            Ok(res) => res,
            Err(_) => {
                log("[SISTEMA] El backend central no responde. Reintentando en 5s...", &state);
                sleep(Duration::from_secs(5)).await;
                continue;
            }
        };

        if !auth_res.status().is_success() { 
            log("[ERROR] Autenticación rechazada. Reintentando en 5s...", &state);
            sleep(Duration::from_secs(5)).await;
            continue; 
        }

        let auth_data: AuthResponse = auth_res.json().await?;
        let token = auth_data.access_token;

        let ctrl_ws_url = format!("ws://{}/api/remote/agent/connect/{}?token={}", backend_host, session_uuid, token);
        log("[TÚNEL] Abriendo canal permanente de control (Idle Mode)...", &state);

        let (ws_stream, _) = match connect_async(ctrl_ws_url).await {
            Ok(ws) => ws,
            Err(e) => {
                log(&format!("[TÚNEL] Error al acoplar socket: {}. Reintentando...", e), &state);
                sleep(Duration::from_secs(3)).await;
                continue;
            }
        };

        let (ctrl_tx, mut ctrl_rx) = ws_stream.split();
        log("[TÚNEL] Conectado con éxito. Guardian en línea y listo.", &state);
        
        {
            let mut lock = state.lock().unwrap();
            lock.estado_conexion = TipoConexion::Inactiva; 
        }

        let ctrl_tx_shared = Arc::new(tokio::sync::Mutex::new(ctrl_tx));
        let ctrl_tx_hb = Arc::clone(&ctrl_tx_shared);
        
        let mut interval = tokio::time::interval(Duration::from_secs(15));
        let (hb_abort_tx, mut hb_abort_rx) = tokio::sync::oneshot::channel::<()>();

        // 🔄 WORKER SECUNDARIO: HEARTBEATS
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut tx_lock = ctrl_tx_hb.lock().await;
                        if tx_lock.send(Message::Text("ping".into())).await.is_err() {
                            break; 
                        }
                    }
                    _ = &mut hb_abort_rx => {
                        break; 
                    }
                }
            }
        });

        while let Some(Ok(msg)) = ctrl_rx.next().await {
            if let Message::Text(text) = msg {
                if text == "pong" {
                    continue; 
                }

                if let Ok(signal) = serde_json::from_str::<ControlSignal>(text.as_str()) {
                    if signal.action == "despertar" {
                        log("[TÚNEL] ¡Señal de despertar recibida! Desplegando canal WebRTC...", &state);
                        
                        let id_pantalla_clone = id_pantalla.clone();
                        let state_session = Arc::clone(&state);
                        let token_session = token.clone();
                        let backend_host_str = backend_host.to_string();
                        let session_id_str = signal.session_uuid.clone();

                        tokio::spawn(async move {
                            if let Err(e) = inicializar_sesion_webrtc(id_pantalla_clone, state_session, token_session, backend_host_str, session_id_str).await {
                                println!("[ERROR SESIÓN] Fallo crítico en sesión WebRTC: {:?}", e);
                            }
                        });
                    }
                }
            }
        }

        let _ = hb_abort_tx.send(()); 
        log("[SISTEMA] Conexión de control caída. Re-estabilizando infraestructura en 3s...", &state);
        sleep(Duration::from_secs(3)).await;
    }
}

// =======================================================================
// 📡 FLUJO DE NEGOCIACIÓN WEBRTC INDEPENDIENTE (HANDSHAKE TRAS DESPERTAR)
// =======================================================================
async fn inicializar_sesion_webrtc(
    id_pantalla: String, 
    state: Arc<Mutex<AppState>>, 
    token: String,
    backend_host: String,
    session_uuid: String
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log = |msg: &str, s: &Arc<Mutex<AppState>>| { s.lock().unwrap().logs.push(msg.to_string()); };

    // 🖥️ 1. Detectamos las pantallas reales/DisplayLink disponibles de forma activa
    #[cfg(target_os = "macos")]
    let pantallas_reales = obtener_pantallas_reales_macos().await;
    #[cfg(not(target_os = "macos"))]
    let pantallas_reales: Vec<String> = vec!["0".to_string()];

    // Mapeamos las opciones legibles para rellenar el dropdown del AppState
    #[cfg(target_os = "macos")]
    let opciones_dropdown: Vec<String> = pantallas_reales.iter().map(|p| p.etiqueta.clone()).collect();
    #[cfg(not(target_os = "macos"))]
    let opciones_dropdown: Vec<String> = pantallas_reales.clone();

    log("[SISTEMA] Levantando pop-up de confirmación en la UI local...", &state);
    {
        let mut lock = state.lock().unwrap();
        // 🔄 2. Sincronizamos las pantallas encontradas con el dropdown de la interfaz gráfica
        lock.pantallas_disponibles = opciones_dropdown.clone();
        if !opciones_dropdown.is_empty() {
            lock.pantalla_seleccionada_inicial = Some(opciones_dropdown[0].clone());
        }
        lock.estado_conexion = TipoConexion::SolicitudPendiente { visor_id: session_uuid.to_string() };
    }

    let mut index_avf_final = id_pantalla;
    
    loop {
        let (respondido, aprobado, etiqueta_seleccionada) = {
            let lock = state.lock().unwrap();
            (lock.respuesta_usuario.is_some(), lock.respuesta_usuario.unwrap_or(false), lock.pantalla_seleccionada_inicial.clone())
        };

        if respondido {
            if !aprobado {
                log("[SISTEMA] Conexión rechazada localmente.", &state);
                return Ok(());
            }
            
            // 🔄 3. Traducimos la etiqueta seleccionada por el usuario al índice AVF numérico real
            #[cfg(target_os = "macos")]
            if let Some(etiqueta) = etiqueta_seleccionada {
                if let Some(encontrada) = pantallas_reales.iter().find(|p| p.etiqueta == etiqueta) {
                    index_avf_final = encontrada.avf_index.clone();
                }
            }
            
            #[cfg(not(target_os = "macos"))]
            if let Some(p) = etiqueta_seleccionada {
                index_avf_final = p;
            }

            log("[SISTEMA] Permiso concedido. Acoplando al canal de señalización WebRTC...", &state);
            break;
        }
        sleep(Duration::from_millis(200)).await;
    }

    // Determinamos si es monitor secundario comparando contra el primer índice disponible
    #[cfg(target_os = "macos")]
    let arranque_secundaria = if !pantallas_reales.is_empty() {
        index_avf_final != pantallas_reales[0].avf_index
    } else {
        false
    };
    
    #[cfg(not(target_os = "macos"))]
    let arranque_secundaria = index_avf_final != "0";
    
    SELECCION_MONITOR_SECUNDARIO.store(arranque_secundaria, Ordering::SeqCst);

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
        state_dc.lock().unwrap().logs.push(format!("[AGENTE] Canal de Datos Abierto por Visor: {}", d.label()));
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
                        let mut real_x = (cmd.x_píxel / cmd.w_nativa) * w as f64;
                        let real_y = (cmd.y_píxel / cmd.h_nativa) * h as f64;
                        
                        if SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst) {
                            real_x += w as f64;
                        }
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
                            tecla_especial => { let _ = enigo.key(tecla_especial, Direction::Press); }
                        }
                    }
                }
                "key_up" => {
                    if let Some(k) = mapear_tecla(&cmd.key) {
                        match k {
                            enigo::Key::Unicode(_) => {}
                            tecla_especial => { let _ = enigo.key(tecla_especial, Direction::Release); }
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

    let track_clone = Arc::clone(&video_track);
    let (tx_reiniciar, mut rx_reiniciar) = tokio::sync::mpsc::channel::<()>(10);
    *REINICIAR_FFMPEG_TX.lock().unwrap() = Some(tx_reiniciar);

    let track_udp_worker = Arc::clone(&track_clone);
    tokio::spawn(async move {
        let listener = UdpSocket::bind("127.0.0.1:5004").await.unwrap();
        loop {
            let mut inbound_buffer = vec![0u8; 2048];
            match listener.recv_from(&mut inbound_buffer).await {
                Ok((n, _)) => { 
                    if n > 0 {
                        let packet_data = inbound_buffer[..n].to_vec();
                        if track_udp_worker.write(&packet_data).await.is_err() { break; } 
                    }
                }
                Err(_) => break,
            }
        }
    });

    let index_avf_inicial_spawn = index_avf_final.clone();

    tokio::spawn(async move {
        let mut primer_arranque = true;
        loop {
            #[cfg(target_os = "macos")]
            let avf_index = if primer_arranque {
                index_avf_inicial_spawn.clone()
            } else {
                let pantallas = obtener_pantallas_reales_macos().await;
                let usar_secundaria = SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst);
                if usar_secundaria && pantallas.len() > 1 {
                    pantallas[1].avf_index.clone()
                } else if !pantallas.is_empty() {
                    pantallas[0].avf_index.clone()
                } else {
                    "0".to_string()
                }
            };

            #[cfg(not(target_os = "macos"))]
            let avf_index = if SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst) { "1".to_string() } else { "0".to_string() };

            primer_arranque = false;
            println!("[HOT-SWAP] Levantando pipeline FFmpeg para monitor (AVF Idx): {}", avf_index);

            // Se añade el flag ":none" en el input para deshabilitar explícitamente micrófonos o cámaras secundarias
            #[cfg(target_os = "macos")]
            let ffmpeg_cmd_string = format!(
                "ffmpeg -nostdin -y -f avfoundation -capture_cursor 1 -pixel_format nv12 -framerate 30 -i \"{}:none\" \
                -r 30 -vf \"scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2:color=black,format=yuv420p\" \
                -vcodec h264_videotoolbox -realtime 1 -tune zerolatency -bf 0 -profile:v baseline -prio_speed 1 \
                -b:v 3500k -maxrate 4000k -bufsize 2000k -g 30 -keyint_min 30 -forced-idr 1 -bsf:v dump_extra -f rtp -payload_type 96 \
                \"rtp://127.0.0.1:5004?pkt_size=1200&buffer_size=10485760\"", avf_index
            );

            #[cfg(target_os = "macos")]
            let mut child = Command::new("sh").arg("-c").arg(&ffmpeg_cmd_string)
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();

            #[cfg(not(target_os = "macos"))]
            let mut child = Command::new("ffmpeg").args(&[
                    "-nostdin", "-y", "-f", "x11grab", "-video_size", "1280x720", "-i", &avf_index,
                    "-r", "60", "-c:v", "h264_v4l2m2m", "-b:v", "3M", "-pix_fmt", "yuv420p",
                    "-f", "rtp", "-payload_type", "96", "rtp://127.0.0.1:5004?pkt_size=1200"
                ]).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();

            tokio::select! {
                _ = rx_reiniciar.recv() => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                status = child.wait() => {
                    println!("[HOT-SWAP] FFmpeg finalizó con estado: {:?}", status);
                }
            }
            sleep(Duration::from_millis(250)).await; 
        }
    });

    let pc_clone = Arc::clone(&pc);
    let state_disconnect = Arc::clone(&state);
    
    while let Some(Ok(msg)) = ws_rx.next().await {
        if let Message::Text(text) = msg {
            if let Ok(payload) = serde_json::from_str::<SignalingMessage>(text.as_str()) {
                if let Some(sdp_data) = payload.sdp {
                    if sdp_data.sdp_type == "answer" {
                        let sdp_json_string = serde_json::json!({"type": "answer", "sdp": sdp_data.sdp}).to_string();
                        if let Ok(rd) = serde_json::from_str::<RTCSessionDescription>(&sdp_json_string) {
                            pc_clone.set_remote_description(rd).await?;
                            log("[AGENTE] Handshake WebRTC completado. Streaming activo.", &state);
                            {
                                let mut lock = state.lock().unwrap();
                                lock.estado_conexion = TipoConexion::Activa;
                            }
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
    
    log("[SISTEMA] Sesión WebRTC finalizada. El agente regresa a espera activa (Idle).", &state_disconnect);
    {
        let mut lock = state_disconnect.lock().unwrap();
        lock.estado_conexion = TipoConexion::Inactiva;
        lock.respuesta_usuario = None; 
    }
    let _ = pc.close().await;
    Ok(())
}