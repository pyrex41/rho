mod app;
mod autocomplete;
mod view;

fn main() -> iced::Result {
    iced::application(app::RhoApp::new, app::RhoApp::update, view::view)
        .title("Rho")
        .theme(view::theme)
        .subscription(app::subscription)
        .default_font(view::FONT_INTER)
        .font(include_bytes!("../fonts/Inter-Regular.ttf").as_slice())
        .font(include_bytes!("../fonts/Inter-Medium.ttf").as_slice())
        .font(include_bytes!("../fonts/Inter-Bold.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Regular.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Medium.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Bold.ttf").as_slice())
        .font(include_bytes!("../fonts/JetBrainsMono-Italic.ttf").as_slice())
        .run()
}
