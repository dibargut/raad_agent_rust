// src/gui.rs
use std::sync::{Arc, Mutex};
use eframe::egui;

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
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            logs: vec!["[SISTEMA] GUI Iniciada. Esperando arranque del Agente...".to_string()],
            estado_conexion: TipoConexion::Inactiva,
            pantalla_seleccionada_inicial: None,
            respuesta_usuario: None,
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

// 🎯 IMPLEMENTACIÓN ADAPTADA A TU VERSIÓN DE EFRAME (Usa fn ui)
impl eframe::App for GuardianGui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Clonamos las variables de control para no retener el lock del Mutex
        let (estado_actual, mut monitor_idx) = {
            let state = self.state.lock().unwrap();
            (state.estado_conexion.clone(), state.pantalla_seleccionada_inicial.clone().unwrap_or_default())
        };

        match estado_actual {
            // 🚨 1. INTERCEPCIÓN POR CONTROL DE ACCESO
            TipoConexion::SolicitudPendiente { visor_id } => {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.add_space(20.0);
                        ui.heading(
                            egui::RichText::new("⚠️ SOLICITUD DE CONTROL REMOTO")
                                .strong()
                                .color(egui::Color32::from_rgb(255, 165, 0))
                        );
                        ui.add_space(10.0);
                        ui.label(format!("Un visor remoto intenta conectar.\nID: {}", visor_id));
                        ui.add_space(20.0);

                        ui.group(|ui| {
                            ui.label("Selecciona el monitor inicial:");
                            ui.horizontal(|ui| {
                                if ui.selectable_value(&mut monitor_idx, "2".to_string(), "🖥️ Monitor Principal (2)").changed() {
                                    let mut state = self.state.lock().unwrap();
                                    state.pantalla_seleccionada_inicial = Some("2".to_string());
                                }
                                if ui.selectable_value(&mut monitor_idx, "3".to_string(), "📺 Monitor Secundario (3)").changed() {
                                    let mut state = self.state.lock().unwrap();
                                    state.pantalla_seleccionada_inicial = Some("3".to_string());
                                }
                            });
                        });

                        ui.add_space(25.0);

                        ui.horizontal(|ui| {
                            if ui.button("✔️ Permitir").clicked() {
                                let mut state = self.state.lock().unwrap();
                                if state.pantalla_seleccionada_inicial.is_none() {
                                    state.pantalla_seleccionada_inicial = Some("2".to_string());
                                }
                                state.respuesta_usuario = Some(true);
                                state.estado_conexion = TipoConexion::Activa;
                            }

                            if ui.button("❌ Rechazar").clicked() {
                                let mut state = self.state.lock().unwrap();
                                state.respuesta_usuario = Some(false);
                                state.estado_conexion = TipoConexion::Inactiva;
                            }
                        });
                    });
                });
            }

            // 🟢 2. INTERFAZ ESTÁNDAR DE CONTROL Y LOGS
            _ => {
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    ui.heading("Guardian Agent - Panel de Control");
                    ui.add_space(10.0);

                    ui.horizontal(|ui| {
                        ui.label("Control de Flujo:");
                        let state = self.state.lock().unwrap();
                        if state.estado_conexion == TipoConexion::Activa {
                            ui.colored_label(egui::Color32::GREEN, "● Transmitiendo en vivo");
                        } else {
                            ui.colored_label(egui::Color32::GRAY, "○ Esperando Visor...");
                        }
                    });

                    ui.add_space(10.0);
                    ui.separator();

                    if estado_actual == TipoConexion::Activa {
                        ui.label("Conmutación en caliente:");
                        ui.horizontal(|ui| {
                            if ui.button("Cambiar a Pantalla 1").clicked() {
                                crate::core::conmutar_pantalla_caliente(false);
                            }
                            if ui.button("Cambiar a Pantalla 2").clicked() {
                                crate::core::conmutar_pantalla_caliente(true);
                            }
                        });
                        ui.add_space(10.0);
                    }

                    ui.label("Logs del Sistema:");
                    egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                        let state = self.state.lock().unwrap();
                        for log in &state.logs {
                            ui.label(log);
                        }
                    });
                });
            }
        }
    }
}

/// 🚀 Lanzador compatible con la inicialización moderna de tu eframe
pub fn lanzar_interfaz(state: Arc<Mutex<AppState>>) -> Result<(), eframe::Error> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "Guardian Agent",
        native_options,
        Box::new(|_cc| Ok(Box::new(GuardianGui::new(state)))),
    )
}