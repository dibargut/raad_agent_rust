// src/gui.rs
use std::sync::{Arc, Mutex};
use eframe::egui;
use enigo::{Enigo, Mouse, Settings};

#[derive(Clone, PartialEq)]
pub enum ModoApp {
    Login,
    PanelControl,
}

#[derive(Clone, PartialEq)]
pub enum TipoConexion {
    Inactiva,
    SolicitudPendiente { visor_id: String, visor_email: String },
    Activa,
}

pub struct AppState {
    // 🔥 NUEVO: Estados para la pantalla de Login
    pub modo: ModoApp,
    pub host_input: String,
    pub serial_input: String,
    pub email_input: String,
    pub pass_input: String,
    pub login_error: Option<String>,
    pub token_jwt: Option<String>,

    pub logs: Vec<String>,
    pub estado_conexion: TipoConexion,
    pub pantalla_seleccionada_inicial: Option<String>,
    pub respuesta_usuario: Option<bool>,
    pub pantallas_disponibles: Vec<String>,
    pub usando_monitor_secundario: bool,
    pub tx_cerrar_conexion: Option<tokio::sync::mpsc::Sender<()>>,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            modo: ModoApp::Login, // Arrancamos bloqueados en el login
            host_input: "192.168.1.135:8080".to_string(),
            serial_input: "test-session-123".to_string(),
            email_input: "".to_string(),
            pass_input: "".to_string(),
            login_error: None,
            token_jwt: None,

            logs: vec!["[SISTEMA] Interfaz iniciada. Esperando autenticación del operador...".to_string()],
            estado_conexion: TipoConexion::Inactiva,
            pantalla_seleccionada_inicial: None,
            respuesta_usuario: None,
            pantallas_disponibles: vec!["0".to_string()], 
            usando_monitor_secundario: false,
            tx_cerrar_conexion: None,
        }
    }
}

pub struct GuardianGui {
    pub state: Arc<Mutex<AppState>>,
}

impl GuardianGui {
    pub fn new(state: Arc<Mutex<AppState>>) -> Self {
        Self { state }
    }
}

impl Drop for GuardianGui {
    fn drop(&mut self) {
        println!("[SISTEMA] Cerrando aplicación limpiamente...");
        #[cfg(not(target_os = "windows"))]
        let _ = std::process::Command::new("killall").arg("ffmpeg").output();
        #[cfg(target_os = "windows")]
        let _ = std::process::Command::new("taskkill").args(&["/F", "/IM", "ffmpeg.exe"]).output();
        std::process::exit(0);
    }
}

impl eframe::App for GuardianGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            #[cfg(not(target_os = "windows"))]
            let _ = std::process::Command::new("killall").arg("ffmpeg").output();
            #[cfg(target_os = "windows")]
            let _ = std::process::Command::new("taskkill").args(&["/F", "/IM", "ffmpeg.exe"]).output();
            std::process::exit(0);
        }

        let modo_actual = { self.state.lock().unwrap().modo.clone() };

        // =======================================================================
        // 🔐 PANTALLA DE LOGIN (Igual que login_screen.dart)
        // =======================================================================
        if modo_actual == ModoApp::Login {
            egui::CentralPanel::default().show_inside(ui, |ui_panel| {
                ui_panel.vertical_centered(|ui_panel| {
                    ui_panel.add_space(40.0);
                    ui_panel.heading(egui::RichText::new("🔐 GUARDIAN AGENT").size(26.0).strong());
                    ui_panel.add_space(5.0);
                    ui_panel.label("SRA Professional Remote Access");
                    ui_panel.add_space(30.0);

                    let mut state = self.state.lock().unwrap();

                    egui::Grid::new("login_grid").spacing([15.0, 15.0]).show(ui_panel, |grid| {
                        grid.label(egui::RichText::new("🌐 Servidor:").strong());
                        grid.text_edit_singleline(&mut state.host_input);
                        grid.end_row();

                        grid.label(egui::RichText::new("💻 Serial UUID:").strong());
                        grid.text_edit_singleline(&mut state.serial_input);
                        grid.end_row();

                        grid.label(egui::RichText::new("👤 Usuario (Email):").strong());
                        grid.text_edit_singleline(&mut state.email_input);
                        grid.end_row();

                        grid.label(egui::RichText::new("🔑 Contraseña:").strong());
                        grid.add(egui::TextEdit::singleline(&mut state.pass_input).password(true));
                        grid.end_row();
                    });

                    ui_panel.add_space(20.0);

                    if let Some(err) = &state.login_error {
                        ui_panel.colored_label(egui::Color32::RED, err);
                        ui_panel.add_space(10.0);
                    }

                    if ui_panel.button(egui::RichText::new("INICIAR SESIÓN").size(16.0).strong()).clicked() {
                        let host = state.host_input.clone();
                        let email = state.email_input.clone();
                        let pass = state.pass_input.clone();
                        let state_clone = Arc::clone(&self.state);
                        
                        state.login_error = None;
                        
                        // Lanzamos la petición HTTP al backend (Exactamente como Flutter)
                        tokio::spawn(async move {
                            let client = reqwest::Client::new();
                            let url = format!("http://{}/api/auth/login", host);
                            
                            match client.post(&url)
                                .json(&serde_json::json!({ "username": email, "password": pass }))
                                .send().await {
                                    Ok(res) if res.status().is_success() => {
                                        if let Ok(json) = res.json::<serde_json::Value>().await {
                                            if let Some(token) = json.get("token").and_then(|v| v.as_str()) {
                                                let mut lock = state_clone.lock().unwrap();
                                                lock.token_jwt = Some(token.to_string());
                                                lock.modo = ModoApp::PanelControl; // Desbloqueamos la UI
                                                lock.logs.push(format!("[SISTEMA] Autenticado en el Tenant como {}.", email));
                                            }
                                        }
                                    }
                                    Ok(res) => {
                                        let mut lock = state_clone.lock().unwrap();
                                        lock.login_error = Some(format!("Error {}: Credenciales incorrectas", res.status()));
                                    }
                                    Err(e) => {
                                        let mut lock = state_clone.lock().unwrap();
                                        lock.login_error = Some(format!("Fallo de red: {}", e));
                                    }
                                }
                        });
                    }
                });
            });
            return; // Detenemos aquí la UI para que no pinte el Panel de Control
        }

        // =======================================================================
        // 🖥️ PANEL DE CONTROL PRINCIPAL (Solo visible tras el Login)
        // =======================================================================
        let (estado_actual, mut monitor_idx, pantallas_reales) = {
            let state = self.state.lock().unwrap();
            (
                state.estado_conexion.clone(),
                state.pantalla_seleccionada_inicial.clone().unwrap_or_default(),
                state.pantallas_disponibles.clone(),
            )
        };

        if let TipoConexion::SolicitudPendiente { .. } = estado_actual {
            if !monitor_idx.is_empty() {
                let usando_secundario = pantallas_reales.len() > 1 && monitor_idx == pantallas_reales[1];
                let (w, _h) = if let Ok(enigo) = Enigo::new(&Settings::default()) {
                    enigo.main_display().unwrap_or((1920, 1080))
                } else {
                    (1920, 1080)
                };

                let pos_x = if usando_secundario { (w * 2) as f32 - 120.0 } else { w as f32 - 120.0 };
                let vp_id = egui::ViewportId::from_hash_of("check_verde_overlay");
                
                let vp_builder = egui::ViewportBuilder::default()
                    .with_title("Guardian Check")
                    .with_transparent(true)          
                    .with_decorations(false)         
                    .with_always_on_top()            
                    .with_mouse_passthrough(true)    
                    .with_position(egui::pos2(pos_x, 40.0))
                    .with_inner_size(egui::vec2(100.0, 100.0)); 

                ctx.show_viewport_immediate(vp_id, vp_builder, |ctx_overlay, _class| {
                    let mut visuals = egui::Visuals::dark();
                    visuals.panel_fill = egui::Color32::TRANSPARENT;
                    visuals.window_fill = egui::Color32::TRANSPARENT;
                    ctx_overlay.set_visuals(visuals);

                    egui::Area::new(egui::Id::new("area_check_verde"))
                        .fixed_pos(egui::pos2(0.0, 0.0))
                        .show(ctx_overlay, |ui_overlay| {
                            ui_overlay.label(egui::RichText::new("✔").size(90.0).color(egui::Color32::GREEN));
                        });
                });
                
                ctx.request_repaint();
            }
        }

        match estado_actual {
            TipoConexion::SolicitudPendiente { visor_id, visor_email } => {
                egui::CentralPanel::default().show_inside(ui, |ui_panel| {
                    ui_panel.vertical_centered(|ui_panel| {
                        ui_panel.add_space(20.0);
                        ui_panel.heading(egui::RichText::new("⚠️ SOLICITUD DE CONTROL REMOTO").strong().color(egui::Color32::from_rgb(255, 165, 0)));
                        ui_panel.add_space(10.0);
                        ui_panel.label(format!("Se ha detectado una conexión entrante.\n\n👤 Usuario: {}\n🔑 Sesión: {}", visor_email, visor_id));
                        ui_panel.add_space(20.0);

                        ui_panel.group(|ui_panel| {
                            ui_panel.label("Selecciona el monitor a proyectar:");
                            ui_panel.horizontal(|ui_panel| {
                                for (i, idx_pantalla) in pantallas_reales.iter().enumerate() {
                                    let texto_boton = if i == 0 { format!("💻 Monitor Principal ({})", idx_pantalla) } else { format!("📺 Monitor Secundario ({})", idx_pantalla) };
                                    if ui_panel.selectable_value(&mut monitor_idx, idx_pantalla.clone(), &texto_boton).changed() {
                                        let mut state = self.state.lock().unwrap();
                                        state.pantalla_seleccionada_inicial = Some(idx_pantalla.clone());
                                    }
                                }
                            });
                        });

                        ui_panel.add_space(25.0);

                        ui_panel.horizontal(|ui_panel| {
                            if ui_panel.button("✔️ Permitir").clicked() {
                                let mut state = self.state.lock().unwrap();
                                if state.pantalla_seleccionada_inicial.is_none() {
                                    state.pantalla_seleccionada_inicial = state.pantallas_disponibles.first().cloned();
                                }
                                if state.pantallas_disponibles.len() > 1 {
                                    let seleccion = state.pantalla_seleccionada_inicial.as_ref().unwrap();
                                    state.usando_monitor_secundario = seleccion == &state.pantallas_disponibles[1];
                                } else {
                                    state.usando_monitor_secundario = false;
                                }
                                state.respuesta_usuario = Some(true);
                                state.estado_conexion = TipoConexion::Activa;
                            }
                            if ui_panel.button("❌ Rechazar").clicked() {
                                let mut state = self.state.lock().unwrap();
                                state.respuesta_usuario = Some(false);
                                state.estado_conexion = TipoConexion::Inactiva;
                            }
                        });
                    });
                });
            }

            _ => {
                egui::CentralPanel::default().show_inside(ui, |ui_panel| {
                    ui_panel.heading("Guardian Agent - Panel de Control");
                    ui_panel.add_space(10.0);

                    ui_panel.horizontal(|ui_panel| {
                        ui_panel.label("Control de Flujo:");
                        let state = self.state.lock().unwrap();
                        if state.estado_conexion == TipoConexion::Activa {
                            ui_panel.colored_label(egui::Color32::GREEN, "● Transmitiendo en vivo");
                        } else {
                            ui_panel.colored_label(egui::Color32::GRAY, "○ Esperando Visor...");
                        }
                    });

                    ui_panel.add_space(10.0);
                    ui_panel.separator();

                    if estado_actual == TipoConexion::Activa {
                        ui_panel.label("Monitor en transmisión (Conmutación en caliente):");
                        ui_panel.add_space(5.0);
                        
                        let usando_secundario = { self.state.lock().unwrap().usando_monitor_secundario };

                        ui_panel.horizontal(|ui_panel| {
                            let mut bg_color_1 = egui::Color32::TRANSPARENT;
                            if !usando_secundario { bg_color_1 = egui::Color32::from_rgba_unmultiplied(0, 255, 0, 40); }

                            ui_panel.group(|ui_panel| {
                                if !usando_secundario { ui_panel.visuals_mut().widgets.inactive.bg_fill = bg_color_1; }
                                if ui_panel.button("💻 Pantalla 1 (Principal)").clicked() {
                                    crate::core::conmutar_pantalla_caliente(false);
                                    self.state.lock().unwrap().usando_monitor_secundario = false;
                                }
                            });

                            if pantallas_reales.len() > 1 {
                                ui_panel.add_space(10.0);
                                let mut bg_color_2 = egui::Color32::TRANSPARENT;
                                if usando_secundario { bg_color_2 = egui::Color32::from_rgba_unmultiplied(0, 255, 0, 40); }

                                ui_panel.group(|ui_panel| {
                                    if usando_secundario { ui_panel.visuals_mut().widgets.inactive.bg_fill = bg_color_2; }
                                    if ui_panel.button("📺 Pantalla 2 (Secundaria)").clicked() {
                                        crate::core::conmutar_pantalla_caliente(true);
                                        self.state.lock().unwrap().usando_monitor_secundario = true;
                                    }
                                });
                            }
                        });
                        ui_panel.add_space(20.0);

                        if ui_panel.button("🛑 Finalizar Transmisión de Pantalla").clicked() {
                            let mut state = self.state.lock().unwrap();
                            if let Some(tx) = state.tx_cerrar_conexion.take() {
                                let _ = tx.try_send(());
                            }
                            state.estado_conexion = TipoConexion::Inactiva;
                            state.logs.push("[SISTEMA] Sesión finalizada manualmente desde el panel de control.".to_string());
                        }
                        ui_panel.add_space(10.0);
                    }

                    ui_panel.label("Logs del Sistema:");
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui_panel, |ui_panel| {
                        let state = self.state.lock().unwrap();
                        for log in &state.logs { ui_panel.label(log); }
                    });
                });
            }
        }
    }
}

pub fn lanzar_interfaz(state: Arc<Mutex<AppState>>) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native("Guardian Agent", native_options, Box::new(|_cc| Ok(Box::new(GuardianGui::new(state)))))
}