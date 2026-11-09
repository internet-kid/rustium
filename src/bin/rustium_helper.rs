//! Helper-процесс для macOS (renderer, GPU, plugin, alerts).
//! Сборщик бандла копирует этот бинарник в «rustium Helper*.app».

fn main() {
    std::process::exit(rustium::run_helper());
}
