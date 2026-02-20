use tray_icon::Icon;

pub(super) const QUIT_ID: &str = "quit";
pub(super) const SHOW_ID: &str = "show";
pub(super) const TOGGLE_ID: &str = "toggle";

pub(super) fn load_icon() -> Icon {
    let icon_bytes = include_bytes!("../../assets/icon_32.png");
    let image = image::load_from_memory(icon_bytes)
        .expect("Failed to load icon asset")
        .into_rgba8();
    let (width, height) = image.dimensions();
    let rgba = image.into_raw();
    Icon::from_rgba(rgba, width, height)
        .unwrap_or_else(|_| Icon::from_rgba(vec![0; 32 * 32 * 4], 32, 32).unwrap())
}
