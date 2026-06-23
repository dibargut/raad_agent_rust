// src/main.rs
mod video;
mod input;
mod gui;
mod core;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Capturamos el mando a distancia (Handle) del runtime de Tokio que está vivo aquí
    let handle = tokio::runtime::Handle::current();

    println!("[MAIN] Inicializando entorno asíncrono y lanzando GUI...");

    // 2. Le pasamos el handle en mano a la interfaz
    if let Err(e) = gui::lanzar_interfaz(handle) {
        eprintln!("[MAIN-ERROR] Error crítico en la interfaz: {}", e);
    }

    Ok(())
}