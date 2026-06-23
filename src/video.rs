// src/video.rs
pub fn detectar_pantallas_sistema() -> Vec<(String, String)> {
    let mut lista = vec![];
    
    #[cfg(target_os = "macos")]
    let output = std::process::Command::new("ffmpeg")
        .args(&["-f", "avfoundation", "-list_devices", "true", "-i", ""])
        .output();

    if let Ok(out) = output {
        let stderr_str = String::from_utf8_lossy(&out.stderr);
        let mut captura_indices = false;
        
        for line in stderr_str.lines() {
            if line.contains("AVFoundation video devices:") {
                captura_indices = true;
                continue;
            }
            if captura_indices && line.contains("AVFoundation audio devices:") {
                break;
            }
            if captura_indices && (line.contains("Capture screen") || line.contains("DisplayLink") || line.contains("Display")) {
                if let Some(pos_open) = line.find('[') {
                    if let Some(pos_close) = line.find(']') {
                        let id = line[pos_open + 1..pos_close].trim().to_string();
                        let nombre = line[pos_close + 1..].trim().to_string();
                        lista.push((id, nombre));
                    }
                }
            }
        }
    }
    
    if lista.is_empty() {
        lista.push(("1".to_string(), "Pantalla por defecto / Principal".to_string()));
        lista.push(("0".to_string(), "Pantalla secundaria".to_string()));
    }
    lista
}