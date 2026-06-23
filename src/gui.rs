// src/gui.rs
use eframe::egui;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use crate::video::detectar_pantallas_sistema;
use crate::core::ejecutar_core_agente;

pub struct AppState {
    pub pantallas: Vec<(String, String)>,
    pub pantalla_seleccionada: Option<String>,
    pub logs: Vec<String>,
}

// 🎯 Ahora la función recibe el handle del runtime de Tokio desde el main
pub fn lanzar_interfaz(tokio_handle: tokio::runtime::Handle) -> Result<(), eframe::Error> {
    // 1. Detectar pantallas de forma síncrona antes de lanzar la GUI
    let pantallas = detectar_pantallas_sistema();

    let state = Arc::new(Mutex::new(AppState {
        pantallas,
        pantalla_seleccionada: None,
        logs: vec!["[GUI] Agente listo. Selecciona una pantalla.".to_string()],
    }));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([450.0, 320.0])
            .with_resizable(false),
        ..Default::default()
    };

    let state_gui = Arc::clone(&state);
    
    eframe::run_simple_native("Guardian Agent - Selector de Pantalla", options, move |ctx, _frame| {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("🛡️ Selector de Pantalla Local");
            ui.label("Selecciona qué pantalla deseas exponer al visor remoto:");
            ui.separator();

            let mut guard = state_gui.lock().unwrap();
            
            if guard.pantalla_seleccionada.is_none() {
                // Clonamos la lista de pantallas para liberar el préstamo inmutable de 'guard' dentro del bucle
                let pantallas_disponibles = guard.pantallas.clone();
                let mut pantalla_a_seleccionar = None;

                egui::ScrollArea::vertical().max_height(140.0).show(ui, |ui| {
                    for (id, nombre) in pantallas_disponibles {
                        if ui.button(format!("🖥️ Mandar Pantalla {} ({})", id, nombre)).clicked() {
                            pantalla_a_seleccionar = Some(id);
                        }
                    }
                });

                // Si el usuario hizo clic en una pantalla, mutamos el estado FUERA del bucle
                if let Some(id) = pantalla_a_seleccionar {
                    guard.pantalla_seleccionada = Some(id.clone());
                    guard.logs.push(format!("[AGENTE] Iniciando transmisión de Pantalla {}", id));
                    
                    let id_clonado = id.clone();
                    let state_tokio = Arc::clone(&state_gui);
                    
                    // 🚀 Usamos el handle inyectado para saltarnos el secuestro de hilos de egui
                    tokio_handle.spawn(async move {
                        if let Err(e) = ejecutar_core_agente(id_clonado, state_tokio).await {
                            println!("[ERROR CORE]: {:?}", e);
                        }
                    });
                }
            } else {
                // Si ya está transmitiendo, mostramos estado de bloqueo seguro
                ui.colored_label(egui::Color32::GREEN, "🟢 Transmitiendo vídeo y control remoto activo...");
                ui.label(format!("Compartiendo actualmente el índice: {}", guard.pantalla_seleccionada.as_ref().unwrap()));
            }

            ui.separator();
            ui.label("Logs del Agente:");
            // Caja de texto que simula la terminal de estado dentro de la UI
            egui::ScrollArea::vertical().max_height(100.0).stick_to_bottom(true).show(ui, |ui| {
                for log in &guard.logs {
                    ui.small(log);
                }
            });
            
            // Forzamos el refresco continuo de la UI para actualizar los logs en tiempo real
            ctx.request_repaint_after(Duration::from_millis(200));
        });
    })
}