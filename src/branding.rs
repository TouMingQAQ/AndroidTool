use eframe::egui;

pub const ICON_PNG: &[u8] = include_bytes!("../assets/androidtool.png");

pub fn window_icon() -> egui::IconData {
    eframe::icon_data::from_png_bytes(ICON_PNG).expect("valid embedded AndroidTool icon")
}

pub fn image(ui: &mut egui::Ui, size: f32) {
    let id = egui::Id::new("androidtool-icon");
    let cached = ui
        .ctx()
        .data(|data| data.get_temp::<egui::TextureHandle>(id));
    let texture = cached.unwrap_or_else(|| {
        let icon = window_icon();
        let texture = ui.ctx().load_texture(
            "androidtool-icon",
            egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            ),
            egui::TextureOptions::LINEAR,
        );
        ui.ctx()
            .data_mut(|data| data.insert_temp(id, texture.clone()));
        texture
    });
    ui.image((texture.id(), egui::Vec2::splat(size)));
}

#[cfg(test)]
mod tests {
    #[test]
    fn sidebar_icon_renders_and_reuses_texture() {
        let ctx = eframe::egui::Context::default();
        for _ in 0..2 {
            let _ = ctx.run(Default::default(), |ctx| {
                eframe::egui::CentralPanel::default().show(ctx, |ui| {
                    super::image(ui, 40.0);
                });
            });
        }
    }

    #[test]
    fn embedded_icon_is_square_with_alpha() {
        let icon = super::window_icon();
        assert_eq!((icon.width, icon.height), (512, 512));
        assert_eq!(icon.rgba.len(), 512 * 512 * 4);
        assert_eq!(icon.rgba[3], 0);
        let center_alpha = ((256 * 512 + 256) * 4) + 3;
        assert_eq!(icon.rgba[center_alpha], 255);
    }
}
