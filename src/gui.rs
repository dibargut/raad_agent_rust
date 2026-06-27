// src/gui.rs
use std::sync::{Arc, Mutex};
use eframe::egui;
// Importamos explícitamente los traits de Enigo
use enigo::{Enigo, Mouse, Settings}; 

#[derive(Clone, PartialEq)]
pub enum TipoConexion {
    Inactiva,
    SolicitudPendiente { visor_id: String },
    Activa,
}

pub struct AppState {
    pub logs: Vec<String>,
    pub estado_conexion: TipoConexion,
    pub pantalla_seleccionada_inicial: Option<String>,
    pub respuesta_usuario: Option<bool>,
    pub pantallas_disponibles: Vec<String>,
    pub usando_monitor_secundario: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            logs: vec!["[SISTEMA] GUI Iniciada. Esperando arranque del Agente...".to_string()],
            estado_conexion: TipoConexion::Inactiva,
            pantalla_seleccionada_inicial: None,
            respuesta_usuario: None,
            pantallas_disponibles: vec!["0".to_string()], 
            usando_monitor_secundario: false,
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

// 🚀 LA GUILLOTINA: Evita el Crash "Abort trap 6" de macOS al cerrar
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
    // 🛠️ Firma alineada exactamente a lo que pide tu compilador
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        
        // Interceptamos la "X" de la ventana para eludir el crash nativo de macOS
        if ctx.input(|i| i.viewport().close_requested()) {
            println!("[SISTEMA] Interceptando cierre nativo para evitar crash de macOS...");
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            
            #[cfg(not(target_os = "windows"))]
            let _ = std::process::Command::new("killall").arg("ffmpeg").output();
            
            #[cfg(target_os = "windows")]
            let _ = std::process::Command::new("taskkill").args(&["/F", "/IM", "ffmpeg.exe"]).output();

            std::process::exit(0);
        }
        
        let (estado_actual, mut monitor_idx, pantallas_reales) = {
            let state = self.state.lock().unwrap();
            (
                state.estado_conexion.clone(),
                state.pantalla_seleccionada_inicial.clone().unwrap_or_default(),
                state.pantallas_disponibles.clone(),
            )
        };

        // =======================================================================
        // 🎯 CHIVATO MINIMALISTA: CHECK VERDE EN LA ESQUINA (Solo visible mientras eliges monitor)
        // =======================================================================
        if let TipoConexion::SolicitudPendiente { .. } = estado_actual {
            if !monitor_idx.is_empty() {
                let usando_secundario = pantallas_reales.len() > 1 && monitor_idx == pantallas_reales[1];
                
                let (w, _h) = if let Ok(enigo) = Enigo::new(&Settings::default()) {
                    enigo.main_display().unwrap_or((1920, 1080))
                } else {
                    (1920, 1080)
                };

                // Esquina superior derecha de la pantalla elegida
                let pos_x = if usando_secundario { (w * 2) as f32 - 120.0 } else { w as f32 - 120.0 };
                
                let vp_id = egui::ViewportId::from_hash_of("check_verde_overlay");
                
                // Ventana minúscula de cristal (100x100)
                let vp_builder = egui::ViewportBuilder::default()
                    .with_title("Guardian Check")
                    .with_transparent(true)          
                    .with_decorations(false)         
                    .with_always_on_top()            
                    .with_mouse_passthrough(true)    
                    .with_position(egui::pos2(pos_x, 40.0)) // Arriba a la derecha
                    .with_inner_size(egui::vec2(100.0, 100.0)); 

                ctx.show_viewport_immediate(vp_id, vp_builder, |ctx_overlay, _class| {
                    let mut visuals = egui::Visuals::dark();
                    visuals.panel_fill = egui::Color32::TRANSPARENT;
                    visuals.window_fill = egui::Color32::TRANSPARENT;
                    ctx_overlay.set_visuals(visuals);

                    egui::Area::new(egui::Id::new("area_check_verde"))
                        .fixed_pos(egui::pos2(0.0, 0.0))
                        .show(ctx_overlay, |ui_overlay| {
                            ui_overlay.label(
                                egui::RichText::new("✔")
                                    .size(90.0)
                                    .color(egui::Color32::GREEN)
                            );
                        });
                });
                
                ctx.request_repaint();
            }
        }

        // =======================================================================
        // 💻 LA VENTANA DE CONTROL PRINCIPAL DE LA APLICACIÓN
        // =======================================================================
        match estado_actual {
            TipoConexion::SolicitudPendiente { visor_id } => {
                // Le pasamos la variable `ui` correctamente como pide el compilador
                egui::CentralPanel::default().show_inside(ui, |ui_panel| {
                    ui_panel.vertical_centered(|ui_panel| {
                        ui_panel.add_space(20.0);
                        ui_panel.heading(
                            egui::RichText::new("⚠️ SOLICITUD DE CONTROL REMOTO")
                                .strong()
                                .color(egui::Color32::from_rgb(255, 165, 0))
                        );
                        ui_panel.add_space(10.0);
                        ui_panel.label(format!("Un visor remoto intenta conectar.\nID: {}", visor_id));
                        ui_panel.add_space(20.0);

                        ui_panel.group(|ui_panel| {
                            ui_panel.label("Selecciona el monitor inicial:");
                            ui_panel.horizontal(|ui_panel| {
                                for (i, idx_pantalla) in pantallas_reales.iter().enumerate() {
                                    let texto_boton = if i == 0 {
                                        format!("💻 Monitor Principal ({})", idx_pantalla)
                                    } else {
                                        format!("📺 Monitor Secundario ({})", idx_pantalla)
                                    };

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
                            if !usando_secundario {
                                bg_color_1 = egui::Color32::from_rgba_unmultiplied(0, 255, 0, 40);
                            }

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
                                if usando_secundario {
                                    bg_color_2 = egui::Color32::from_rgba_unmultiplied(0, 255, 0, 40);
                                }

                                ui_panel.group(|ui_panel| {
                                    if usando_secundario { ui_panel.visuals_mut().widgets.inactive.bg_fill = bg_color_2; }
                                    if ui_panel.button("📺 Pantalla 2 (Secundaria)").clicked() {
                                        crate::core::conmutar_pantalla_caliente(true);
                                        self.state.lock().unwrap().usando_monitor_secundario = true;
                                    }
                                });
                            }
                        });
                        ui_panel.add_space(10.0);
                    }

                    ui_panel.label("Logs del Sistema:");
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui_panel, |ui_panel| {
                        let state = self.state.lock().unwrap();
                        for log in &state.logs {
                            ui_panel.label(log);
                        }
                    });
                });
            }
        }
    }
}

pub fn lanzar_interfaz(state: Arc<Mutex<AppState>>) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guardian Agent",
        native_options,
        Box::new(|_cc| Ok(Box::new(GuardianGui::new(state)))),
    )
}