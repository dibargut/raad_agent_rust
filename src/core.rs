// src/core.rs
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::net::UdpSocket;
use tokio::process::Command;
use tokio::time::{sleep, Duration};
use tokio::io::AsyncWriteExt; 
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

use webrtc::api::media_engine::{MediaEngine, MIME_TYPE_H264};
use webrtc::api::APIBuilder;
use enigo::{Direction, Enigo, Keyboard, Mouse, Settings};
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::{rtp_codec::RTCRtpCodecCapability, rtp_transceiver_direction::RTCRtpTransceiverDirection};
use webrtc::track::track_local::track_local_static_rtp::TrackLocalStaticRTP;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;

use crate::gui::{AppState, TipoConexion, ModoApp};
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
    #[serde(default)] text: String, // 🔥 NUEVO: Campo para recibir el texto del portapapeles
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

#[derive(Deserialize, Debug)]
struct ControlSignal { 
    action: String, 
    session_uuid: Option<String>,
    visor_email: Option<String> 
}

// =======================================================================
// 🖥️ DETECCIÓN ADAPTATIVA DE PANTALLAS EN MACOS
// =======================================================================

#[cfg(target_os = "macos")]
#[derive(Debug, Clone)]
pub struct PantallaReal {
    pub avf_index: String,
    pub etiqueta: String,
}

#[cfg(target_os = "macos")]
pub async fn obtener_pantallas_reales_macos() -> Vec<PantallaReal> {
    let output = Command::new("ffmpeg")
        .args(&["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output()
        .await;

    let mut pantallas_detectadas = Vec::new();
    let palabras_camara = ["facetime", "webcam", "camera", "iphone", "ipad", "continuity"];

    if let Ok(out) = output {
        let stderr_txt = String::from_utf8_lossy(&out.stderr);
        let bloque_video = stderr_txt.split("AVFoundation audio devices").next().unwrap_or(&stderr_txt);

        for line in bloque_video.lines() {
            let linea_lower = line.to_lowercase();
            
            if linea_lower.contains("capture screen") || linea_lower.contains("capture_screen") {
                if palabras_camara.iter().any(|kw| linea_lower.contains(kw)) { continue; }

                if let Some(pos_dispositivo) = line.find(']') {
                    let resto_linea = &line[pos_dispositivo + 1..];
                    if let (Some(start), Some(end)) = (resto_linea.find('['), resto_linea.find(']')) {
                        let idx = resto_linea[start + 1..end].trim().to_string();
                        if !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit()) {
                            if !pantallas_detectadas.iter().any(|p: &PantallaReal| p.avf_index == idx) {
                                pantallas_detectadas.push(PantallaReal { avf_index: idx, etiqueta: String::new() });
                            }
                        }
                    }
                }
            }
        }
    }

    if pantallas_detectadas.is_empty() {
        let output_pantallas = Command::new("osascript").args(&["-e", "tell application \"Finder\" to get count of screens"]).output().await;
        let mut conteo = 1;
        if let Ok(out) = output_pantallas {
            if let Ok(num) = String::from_utf8_lossy(&out.stdout).trim().parse::<usize>() { conteo = num; }
        }
        for i in 0..conteo { pantallas_detectadas.push(PantallaReal { avf_index: i.to_string(), etiqueta: String::new() }); }
    }

    for (i, pantalla) in pantallas_detectadas.iter_mut().enumerate() {
        pantalla.etiqueta = if i == 0 { format!("Monitor Principal (Pantalla 1) [AVF idx {}]", pantalla.avf_index) } 
        else { format!("Monitor Externo (Pantalla {}) [AVF idx {}]", i + 1, pantalla.avf_index) };
    }
    pantallas_detectadas
}

#[cfg(target_os = "macos")]
pub async fn obtener_indices_pantalla_macos() -> Vec<String> {
    obtener_pantallas_reales_macos().await.into_iter().map(|p| p.avf_index).collect()
}

pub fn conmutar_pantalla_caliente(hacia_secundaria: bool) {
    SELECCION_MONITOR_SECUNDARIO.store(hacia_secundaria, Ordering::SeqCst);
    if let Some(tx) = &*REINICIAR_FFMPEG_TX.lock().unwrap() {
        let tx_clone = tx.clone();
        tokio::spawn(async move { let _ = tx_clone.send(()).await; });
    }
}

// =======================================================================
// 📂 DESCARGA BINARIA EN TIEMPO REAL (CONSUMO DEL ENDPOINT B)
// =======================================================================
async fn descargar_archivo_binario(url: String, filename: String, state: Arc<Mutex<AppState>>) {
    let client = reqwest::Client::new();
    let dir_path = "./guardian_downloads";
    
    let _ = tokio::fs::create_dir_all(dir_path).await;
    let filepath = format!("{}/{}", dir_path, filename);
    
    state.lock().unwrap().logs.push(format!("[STREAM] Recibiendo y ensamblando binario: {}", filename));

    match client.get(&url).send().await {
        Ok(mut res) => {
            if res.status().is_success() {
                if let Ok(mut file) = tokio::fs::File::create(&filepath).await {
                    let mut bytes_total = 0;
                    while let Ok(Some(chunk)) = res.chunk().await {
                        if file.write_all(&chunk).await.is_ok() { bytes_total += chunk.len(); }
                    }
                    state.lock().unwrap().logs.push(format!("[STREAM] ✅ Archivo '{}' guardado ({} bytes).", filename, bytes_total));
                } else {
                    state.lock().unwrap().logs.push(format!("[ERROR] No se pudo crear el archivo local {}", filename));
                }
            } else {
                state.lock().unwrap().logs.push(format!("[ERROR] Servidor rechazó la descarga: {}", res.status()));
            }
        }
        Err(e) => {
            state.lock().unwrap().logs.push(format!("[STREAM ERROR] Conexión interrumpida en {}: {}", filename, e));
        }
    }
}

// =======================================================================
// 📡 VIGILANTE SSE (SERVER-SENT EVENTS) - ENDPOINTS C y D
// =======================================================================
async fn iniciar_centinela_sse(session_uuid: String, backend_host: String, state: Arc<Mutex<AppState>>) {
    let client = reqwest::Client::new();
    let url = format!("http://{}/api/watcher/stream-alerts/{}", backend_host, session_uuid);

    loop {
        if let Ok(mut res) = client.get(&url).send().await {
            let mut buffer = String::new();
            while let Ok(Some(chunk)) = res.chunk().await {
                if let Ok(text) = std::str::from_utf8(&chunk) {
                    buffer.push_str(text);
                    while let Some(pos) = buffer.find('\n') {
                        let line = buffer[..pos].to_string();
                        buffer = buffer[pos+1..].to_string();
                        
                        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&line) {
                            if let Some(event) = json.get("event").and_then(|v| v.as_str()) {
                                if event == "alert" || event == "clean" {
                                    if let Some(payload) = json.get("payload").and_then(|v| v.as_str()) {
                                        if payload.starts_with("INCOMING_STREAM:") {
                                            let filename = payload.replace("INCOMING_STREAM:", "");
                                            let dl_url = format!("http://{}/api/watcher/stream-download/{}/{}", backend_host, session_uuid, filename);
                                            let dl_state = Arc::clone(&state);
                                            tokio::spawn(async move {
                                                descargar_archivo_binario(dl_url, filename, dl_state).await;
                                            });
                                        } else if payload == "COMMAND:CLEAN_STORAGE" {
                                            let _ = tokio::fs::remove_dir_all("./guardian_downloads").await;
                                            let _ = tokio::fs::create_dir_all("./guardian_downloads").await;
                                            state.lock().unwrap().logs.push("[ALMACENAMIENTO] 🧹 Purga de directorio temporal completada.".to_string());
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        sleep(Duration::from_secs(5)).await;
    }
}

// =======================================================================
// 🔌 FUNCIÓN PRINCIPAL: TÚNEL DE CONTROL PERSISTENTE (IDLE)
// =======================================================================
pub async fn ejecutar_core_agente(id_pantalla: String, state: Arc<Mutex<AppState>>) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log = |msg: &str, s: &Arc<Mutex<AppState>>| { s.lock().unwrap().logs.push(msg.to_string()); };

    // 🔥 1. ESPERAMOS A QUE EL OPERADOR HAGA LOGIN EN LA UI
    // Rust nos permite retornar los valores directamente desde el bucle para evitar declarar variables vacías.
    let (backend_host, session_uuid, token) = loop {
        {
            let lock = state.lock().unwrap();
            // Si la UI pasó al Panel de Control y tenemos un Token, sacamos los datos
            if lock.modo == ModoApp::PanelControl {
                if let Some(t) = &lock.token_jwt {
                    break (
                        lock.host_input.clone(),
                        lock.serial_input.clone(),
                        t.clone()
                    );
                }
            }
        }
        // Dormimos el bucle asíncrono para no saturar la CPU mientras el usuario escribe
        sleep(Duration::from_millis(500)).await;
    };

    log(&format!("[TÚNEL] Iniciando núcleo para el dispositivo: {}", session_uuid), &state);

    // 🔥 2. INYECTAMOS EL VIGILANTE SSE CON LOS DATOS REALES (Sin Hardcodings)
    let sse_session = session_uuid.clone();
    let sse_host = backend_host.clone();
    let sse_state = Arc::clone(&state);
    tokio::spawn(async move { iniciar_centinela_sse(sse_session, sse_host, sse_state).await; });

    // 🔥 3. BUCLE PRINCIPAL DE CONEXIÓN WEBSOCKET 
    loop {
        let ctrl_ws_url = format!("ws://{}/api/remote/agent/connect/{}?token={}", backend_host, session_uuid, token);
        log("[TÚNEL] Abriendo canal permanente de control (Idle Mode)...", &state);

        let (ws_stream, _) = match connect_async(&ctrl_ws_url).await {
            Ok(ws) => ws,
            Err(e) => {
                log(&format!("[TÚNEL] Error al acoplar socket: {}. Reintentando en 5s...", e), &state);
                sleep(Duration::from_secs(5)).await;
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

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = interval.tick() => {
                        let mut tx_lock = ctrl_tx_hb.lock().await;
                        if tx_lock.send(Message::Text("ping".into())).await.is_err() { break; }
                    }
                    _ = &mut hb_abort_rx => { break; }
                }
            }
        });

        while let Some(Ok(msg)) = ctrl_rx.next().await {
            let texto_recibido = match msg {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => String::from_utf8_lossy(&b).to_string(),
                _ => continue,
            };

            if texto_recibido == "pong" { continue; }

            match serde_json::from_str::<ControlSignal>(&texto_recibido) {
                Ok(signal) => {
                    if signal.action == "despertar" {
                        log("[TÚNEL] ¡Señal de despertar recibida! Desplegando canal WebRTC...", &state);
                        
                        let id_pantalla_clone = id_pantalla.clone();
                        let state_session = Arc::clone(&state);
                        let token_session = token.clone();
                        let backend_host_str = backend_host.clone();
                        let session_id_str = signal.session_uuid.unwrap_or_else(|| session_uuid.clone());
                        let email_str = signal.visor_email.unwrap_or_else(|| "desconocido@guardian.com".to_string());

                        tokio::spawn(async move {
                            if let Err(e) = inicializar_sesion_webrtc(id_pantalla_clone, state_session, token_session, backend_host_str, session_id_str, email_str).await {
                                println!("[ERROR SESIÓN] Fallo crítico en sesión WebRTC: {:?}", e);
                            }
                        });
                    } 
                    else if signal.action == "mfa_authorized" {
                        log("[MFA] Autorización remota recibida. Aceptando conexión automáticamente...", &state);
                        let mut lock = state.lock().unwrap();
                        
                        if let TipoConexion::SolicitudPendiente { .. } = lock.estado_conexion {
                            if lock.pantalla_seleccionada_inicial.is_none() {
                                lock.pantalla_seleccionada_inicial = lock.pantallas_disponibles.first().cloned();
                            }
                            if lock.pantallas_disponibles.len() > 1 {
                                let seleccion = lock.pantalla_seleccionada_inicial.as_ref().unwrap();
                                lock.usando_monitor_secundario = seleccion == &lock.pantallas_disponibles[1];
                            } else {
                                lock.usando_monitor_secundario = false;
                            }
                            lock.respuesta_usuario = Some(true);
                            lock.estado_conexion = TipoConexion::Activa;
                        }
                    }
                    else if signal.action == "end_session" {
                        log("[SISTEMA] 🛑 Orden de apagado remoto recibida. Cortando transmisión...", &state);
                        let mut lock = state.lock().unwrap();
                        if let Some(tx) = lock.tx_cerrar_conexion.take() {
                            let _ = tx.try_send(());
                        }
                        lock.estado_conexion = TipoConexion::Inactiva;
                    }
                }
                Err(e) => { println!("🚨 [JSON ERROR] Error de parseo: {:?}", e); }
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
    session_uuid: String,
    visor_email: String
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let log = |msg: &str, s: &Arc<Mutex<AppState>>| { s.lock().unwrap().logs.push(msg.to_string()); };

    #[cfg(target_os = "macos")]
    let pantallas_reales = obtener_pantallas_reales_macos().await;
    #[cfg(not(target_os = "macos"))]
    let pantallas_reales: Vec<String> = vec!["0".to_string()];

    #[cfg(target_os = "macos")]
    let opciones_dropdown: Vec<String> = pantallas_reales.iter().map(|p| p.etiqueta.clone()).collect();
    #[cfg(not(target_os = "macos"))]
    let opciones_dropdown: Vec<String> = pantallas_reales.clone();

    let (tx_corte_gui, mut rx_corte_gui) = tokio::sync::mpsc::channel::<()>(1);

    log("[SISTEMA] Levantando pop-up de confirmación en la UI local...", &state);
    {
        let mut lock = state.lock().unwrap();
        lock.pantallas_disponibles = opciones_dropdown.clone();
        if !opciones_dropdown.is_empty() {
            lock.pantalla_seleccionada_inicial = Some(opciones_dropdown[0].clone());
        }
        lock.estado_conexion = TipoConexion::SolicitudPendiente { 
            visor_id: session_uuid.to_string(), 
            visor_email: visor_email.clone() 
        };
        lock.tx_cerrar_conexion = Some(tx_corte_gui);
    }

    let mut index_avf_final = id_pantalla;
    
    loop {
        if let Ok(_) = rx_corte_gui.try_recv() { return Ok(()); }

        let (respondido, aprobado, etiqueta_seleccionada) = {
            let lock = state.lock().unwrap();
            (lock.respuesta_usuario.is_some(), lock.respuesta_usuario.unwrap_or(false), lock.pantalla_seleccionada_inicial.clone())
        };

        if respondido {
            if !aprobado {
                log("[SISTEMA] Conexión rechazada localmente.", &state);
                return Ok(());
            }
            
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

    #[cfg(target_os = "macos")]
    let arranque_secundaria = if !pantallas_reales.is_empty() {
        index_avf_final != pantallas_reales[0].avf_index
    } else { false };
    
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

    let (tx_kill_workers, _) = tokio::sync::broadcast::channel::<()>(1);
    
    let mut rx_kill_mouse = tx_kill_workers.subscribe();
    std::thread::spawn(move || {
        let mut enigo = match Enigo::new(&Settings::default()) { Ok(e) => e, Err(_) => return };
        let (mut w, mut h) = enigo.main_display().unwrap_or((1920, 1080));
        let mut ultima_seleccion_secundaria = SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst);

        while let Some(cmd) = rx_control.blocking_recv() {
            if rx_kill_mouse.try_recv().is_ok() { break; } 

            let modo_actual_secundaria = SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst);
            if modo_actual_secundaria != ultima_seleccion_secundaria {
                if let Ok(nueva_enigo) = Enigo::new(&Settings::default()) {
                    enigo = nueva_enigo;
                    if let Ok(dim) = enigo.main_display() { w = dim.0; h = dim.1; }
                }
                ultima_seleccion_secundaria = modo_actual_secundaria;
            }

            match cmd.event.as_str() {
                "mouse_move" => {
                    if cmd.w_nativa > 0.0 && cmd.h_nativa > 0.0 {
                        let mut real_x = (cmd.x_píxel / cmd.w_nativa) * w as f64;
                        let real_y = (cmd.y_píxel / cmd.h_nativa) * h as f64;
                        if modo_actual_secundaria { real_x += w as f64; }
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
                // 🔥 NUEVO: INYECCIÓN INTELIGENTE DE PORTAPAPELES (Agnóstico al SO)
                "clipboard_inject" => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        if clipboard.set_text(cmd.text.clone()).is_ok() {
                            println!("[PORTAPAPELES] Texto copiado desde el visor remoto con éxito.");
                            
                            // 1. 🔥 Pausa obligatoria: Le da tiempo al SO (especialmente a Mac) a asimilar la nueva memoria
                            std::thread::sleep(std::time::Duration::from_millis(150));
                            
                            // 2. Limpieza de seguridad: Soltamos teclas que pudieran haber quedado "pegadas"
                            let _ = enigo.key(enigo::Key::Control, Direction::Release);
                            let _ = enigo.key(enigo::Key::Meta, Direction::Release);
                            let _ = enigo.key(enigo::Key::Shift, Direction::Release);

                            // 3. El compilador de Rust elige el bloque correcto según el PC donde corra el Agente
                            #[cfg(target_os = "macos")]
                            {
                                // Lógica exclusiva para MAC (Command + V físico)
                                let _ = enigo.key(enigo::Key::Meta, Direction::Press);
                                let _ = enigo.key(enigo::Key::Unicode('v'), Direction::Press);
                                std::thread::sleep(std::time::Duration::from_millis(50));
                                let _ = enigo.key(enigo::Key::Unicode('v'), Direction::Release);
                                let _ = enigo.key(enigo::Key::Meta, Direction::Release);
                            }
                            #[cfg(not(target_os = "macos"))]
                            {
                                // Lógica exclusiva para Windows y Linux (Ctrl + V físico)
                                let _ = enigo.key(enigo::Key::Control, Direction::Press);
                                let _ = enigo.key(enigo::Key::Unicode('v'), Direction::Press);
                                std::thread::sleep(std::time::Duration::from_millis(50));
                                let _ = enigo.key(enigo::Key::Unicode('v'), Direction::Release);
                                let _ = enigo.key(enigo::Key::Control, Direction::Release);
                            }
                        }
                    }
                }
                // 🔥 NUEVO: SINCRONIZACIÓN SILENCIOSA PARA EL CLICK DERECHO
                // 🔥 ROLLBACK & CLEAN: Solo actualizamos la memoria silenciosamente
                "clipboard_sync" => {
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        if clipboard.set_text(cmd.text.clone()).is_ok() {
                            println!("[PORTAPAPELES] Texto recibido del modal y guardado en la memoria de la Mac.");
                            // No simulamos teclado. El usuario usará Click Derecho -> Pegar, o Cmd+V desde el visor.
                        }
                    }
                }
                _ => {}
            }
        }
    });

    let state_wrtc = Arc::clone(&state);
    let (tx_end_session, mut rx_end_session) = tokio::sync::mpsc::channel::<()>(1);
    let tx_end_clone = tx_end_session.clone();
    
    pc.on_peer_connection_state_change(Box::new(move |estado_webrtc| {
        state_wrtc.lock().unwrap().logs.push(format!("[WEBRTC] Estado: {}", estado_webrtc));
        if estado_webrtc == RTCPeerConnectionState::Failed || estado_webrtc == RTCPeerConnectionState::Disconnected || estado_webrtc == RTCPeerConnectionState::Closed {
            let _ = tx_end_clone.try_send(());
        }
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
    let mut rx_kill_udp = tx_kill_workers.subscribe();
    
    tokio::spawn(async move {
        if let Ok(listener) = UdpSocket::bind("127.0.0.1:5004").await {
            let mut inbound_buffer = vec![0u8; 2048];
            let mut current_ssrc = 0u32;
            let mut first_ssrc_ever = 0u32;
            let mut seq_offset = 0u16;
            let mut ts_offset = 0u32;
            let mut last_seq_out = 0u16;
            let mut last_ts_out = 0u32;
            let mut is_first_packet = true;

            loop {
                tokio::select! {
                    _ = rx_kill_udp.recv() => { break; } 
                    res = listener.recv_from(&mut inbound_buffer) => {
                        if let Ok((n, _)) = res {
                            if n >= 12 { 
                                let mut packet_data = inbound_buffer[..n].to_vec();
                                let seq_in = u16::from_be_bytes([packet_data[2], packet_data[3]]);
                                let ts_in = u32::from_be_bytes([packet_data[4], packet_data[5], packet_data[6], packet_data[7]]);
                                let ssrc_in = u32::from_be_bytes([packet_data[8], packet_data[9], packet_data[10], packet_data[11]]);

                                if is_first_packet {
                                    first_ssrc_ever = ssrc_in;
                                    current_ssrc = ssrc_in;
                                    is_first_packet = false;
                                } else if ssrc_in != current_ssrc {
                                    seq_offset = last_seq_out.wrapping_sub(seq_in).wrapping_add(1);
                                    ts_offset = last_ts_out.wrapping_sub(ts_in).wrapping_add(3000); 
                                    current_ssrc = ssrc_in;
                                }

                                let seq_out = seq_in.wrapping_add(seq_offset);
                                let ts_out = ts_in.wrapping_add(ts_offset);
                                last_seq_out = seq_out;
                                last_ts_out = ts_out;

                                packet_data[2..4].copy_from_slice(&seq_out.to_be_bytes());
                                packet_data[4..8].copy_from_slice(&ts_out.to_be_bytes());
                                packet_data[8..12].copy_from_slice(&first_ssrc_ever.to_be_bytes());

                                if track_udp_worker.write(&packet_data).await.is_err() { break; } 
                            } else if n > 0 {
                                let packet_data = inbound_buffer[..n].to_vec();
                                if track_udp_worker.write(&packet_data).await.is_err() { break; } 
                            }
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    });

    let index_avf_inicial_spawn = index_avf_final.clone();
    let mut rx_kill_ffmpeg = tx_kill_workers.subscribe();

    tokio::spawn(async move {
        let mut primer_arranque = true;
        loop {
            #[cfg(target_os = "macos")]
            let avf_index = if primer_arranque {
                index_avf_inicial_spawn.clone()
            } else {
                let pantallas = obtener_pantallas_reales_macos().await;
                let usar_secundaria = SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst);
                if usar_secundaria && pantallas.len() > 1 { pantallas[1].avf_index.clone() } 
                else if !pantallas.is_empty() { pantallas[0].avf_index.clone() } 
                else { "0".to_string() }
            };

            #[cfg(not(target_os = "macos"))]
            let avf_index = if SELECCION_MONITOR_SECUNDARIO.load(Ordering::SeqCst) { "1".to_string() } else { "0".to_string() };

            primer_arranque = false;

            #[cfg(target_os = "macos")]
            let input_arg = format!("{}:none", avf_index);
            
            #[cfg(target_os = "macos")]
            let mut child = Command::new("ffmpeg")
                .args(&[
                    "-nostdin", "-y", "-f", "avfoundation", "-capture_cursor", "1",
                    "-pixel_format", "nv12", "-framerate", "30", "-i", &input_arg,
                    "-r", "30",
                    "-vf", "scale=1280:720:force_original_aspect_ratio=decrease,pad=1280:720:(ow-iw)/2:(oh-ih)/2:color=black,format=yuv420p",
                    "-vcodec", "h264_videotoolbox", "-realtime", "1", "-tune", "zerolatency",
                    "-bf", "0", "-profile:v", "baseline", "-prio_speed", "1",
                    "-b:v", "3500k", "-maxrate", "4000k", "-bufsize", "2000k",
                    "-g", "30", "-keyint_min", "30", "-forced-idr", "1",
                    "-bsf:v", "dump_extra", "-f", "rtp", "-payload_type", "96",
                    "rtp://127.0.0.1:5004?pkt_size=1200&buffer_size=10485760"
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().unwrap();

            #[cfg(not(target_os = "macos"))]
            let mut child = Command::new("ffmpeg")
                .args(&[
                    "-nostdin", "-y", "-f", "x11grab", "-video_size", "1280x720", "-i", &avf_index,
                    "-r", "60", "-c:v", "h264_v4l2m2m", "-b:v", "3M", "-pix_fmt", "yuv420p",
                    "-f", "rtp", "-payload_type", "96", "rtp://127.0.0.1:5004?pkt_size=1200"
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn().unwrap();

            tokio::select! {
                _ = rx_kill_ffmpeg.recv() => { 
                    println!("[SISTEMA] Cerrando proceso FFmpeg de forma segura...");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                    break;
                }
                _ = rx_reiniciar.recv() => { 
                    println!("[HOT-SWAP] Reiniciando FFmpeg para cambio de pantalla...");
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
                status = child.wait() => { println!("[HOT-SWAP] FFmpeg finalizó inesperadamente con estado: {:?}", status); }
            }
            sleep(Duration::from_millis(250)).await; 
        }
    });

    let pc_clone = Arc::clone(&pc);
    let state_disconnect = Arc::clone(&state);
    
    loop {
        tokio::select! {
            _ = rx_corte_gui.recv() => {
                log("[SISTEMA] Apagado forzado desde el Agente.", &state);
                break;
            }
            _ = rx_end_session.recv() => {
                log("[SISTEMA] El Visor cortó la conexión abruptamente.", &state);
                break;
            }
            msg = ws_rx.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(payload) = serde_json::from_str::<SignalingMessage>(&text) {
                            if let Some(sdp_data) = payload.sdp {
                                if sdp_data.sdp_type == "answer" {
                                    let sdp_json_string = serde_json::json!({"type": "answer", "sdp": sdp_data.sdp}).to_string();
                                    if let Ok(rd) = serde_json::from_str::<RTCSessionDescription>(&sdp_json_string) {
                                        let _ = pc_clone.set_remote_description(rd).await;
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
                    },
                    Some(Err(_)) | None => {
                        log("[SISTEMA] Canal WebSocket del visor cerrado.", &state);
                        break;
                    },
                    _ => {}
                }
            }
        }
    }
    
    log("[SISTEMA] Limpiando procesos en segundo plano y volviendo a Idle...", &state_disconnect);
    let _ = tx_kill_workers.send(()); 
    
    {
        let mut lock = state_disconnect.lock().unwrap();
        lock.estado_conexion = TipoConexion::Inactiva;
        lock.respuesta_usuario = None; 
    }
    
    let _ = pc.close().await;
    Ok(())
}