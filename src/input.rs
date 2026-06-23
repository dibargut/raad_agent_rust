// src/input.rs
use enigo::Key;

pub fn mapear_tecla(key_str: &str) -> Option<Key> {
    match key_str {
        "Enter" => Some(Key::Return),
        "Backspace" => Some(Key::Backspace),
        "Tab" => Some(Key::Tab),
        "Escape" => Some(Key::Escape),
        "Space" | " " => Some(Key::Space),
        "ArrowUp" => Some(Key::UpArrow),
        "ArrowDown" => Some(Key::DownArrow),
        "ArrowLeft" => Some(Key::LeftArrow),
        "ArrowRight" => Some(Key::RightArrow),
        "Meta" | "Command" => Some(Key::Meta),
        "Shift" => Some(Key::Shift),
        "Control" => Some(Key::Control),
        "Alt" => Some(Key::Alt),
        s if s.len() == 1 => s.chars().next().map(Key::Unicode),
        _ => None,
    }
}