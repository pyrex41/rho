mod app;
mod view;

fn main() -> iced::Result {
    iced::application(app::RhoApp::new, app::RhoApp::update, view::view)
        .title("Rho")
        .theme(view::theme)
        .subscription(app::subscription)
        .run()
}
