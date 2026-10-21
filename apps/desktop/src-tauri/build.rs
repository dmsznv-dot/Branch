fn main() {
    #[cfg(feature = "native-host")]
    tauri_build::try_build(tauri_build::Attributes::new().app_manifest(
        tauri_build::AppManifest::new().commands(&[
            "choose_media",
            "player_command",
            "set_fullscreen",
            "player_snapshot",
        ]),
    ))
    .expect("desktop build configuration");
}
