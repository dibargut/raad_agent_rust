// src/main.rs
use std::sync::{Arc, Mutex};
use tokio;

mod core;
mod gui;
mod input;

#[tokio::main]
async fn main() {
    // 1. 📂 Inicializamos el estado compartido que usarán la GUI y el Core multimedia
    let state = Arc::new(Mutex::new(gui::AppState::default()));

    // 2. 🚀 Clonamos el puntero atómico para pasárselo al hilo asíncronico del agente
    let state_core = Arc::clone(&state);
    
    tokio::spawn(async move {
        // Pasamos "0" como fallback inicial. La app detectará los monitores reales dinámicamente al despertar.
        if let Err(e) = crate::core::ejecutar_core_agente("0".to_string(), state_core).await {
            eprintln!("[ERROR CORE] El motor multimedia falló: {:?}", e);
        }
    });

    // 3. 🎨 Lanzamos la interfaz gráfica en el hilo principal pasándole el estado
    println!("[SISTEMA] Iniciando ventana nativa de eframe...");
    if let Err(e) = gui::lanzar_interfaz(state) {
        eprintln!("[ERROR GUI] No se pudo arrancar la interfaz gráfica: {:?}", e);
    }
}