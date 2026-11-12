//! 设置(P-60、P-67)。范围、能不能保存、错误文案都读 `App`(`SettingsEditor`),这里只画。

use gpui_kit::TestSupportExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::InputState;
use gpui_kit::component::{Disableable as _, Selectable as _, h_flex, v_flex};
use gpui_kit::*;
use xredis_app::Msg;
use xredis_app::i18n::available_languages;
use xredis_app::msg::SettingsText;

use super::dialogs::{bind_input, error_list, field, new_input};
use super::send_to;
use crate::model::MainView;

pub(crate) struct SettingsBody {
    main: Entity<MainView>,
    batch: Entity<InputState>,
    auto_scan: Entity<InputState>,
    languages: Vec<String>,
    _subs: Vec<Subscription>,
}

impl SettingsBody {
    pub(crate) fn new(
        main: Entity<MainView>,
        app: &xredis_app::App,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = app.settings_editor.clone().expect("settings are open");
        let batch = new_input(window, cx, &draft.scan_batch, "", false);
        let auto_scan = new_input(window, cx, &draft.auto_scan_limit, "", false);
        let mut subs = vec![cx.observe(&main, |_, _, cx| cx.notify())];
        for (input, field) in [
            (&batch, SettingsText::ScanBatch),
            (&auto_scan, SettingsText::AutoScanLimit),
        ] {
            subs.push(bind_input(
                window,
                cx,
                &main,
                input,
                move |text| Msg::SettingsSetText(field, text),
                || Msg::SettingsSave,
            ));
        }
        Self {
            main,
            batch,
            auto_scan,
            languages: available_languages(),
            _subs: subs,
        }
    }

    pub(crate) fn first_input(&self) -> Entity<InputState> {
        self.batch.clone()
    }
}

impl Render for SettingsBody {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = self.main.clone();
        let app = &main.read(cx).app;
        let Some(e) = &app.settings_editor else {
            return div().into_any_element();
        };
        let t = |k: &str| app.t(k);
        let errors: Vec<String> = e
            .errors(&app.settings)
            .into_iter()
            .map(|f| f.text(&app.lang))
            .collect();

        let languages = self.languages.iter().map(|code| {
            let wanted = code.clone();
            Button::new(SharedString::from(format!("settings-lang-{code}")))
                .compact()
                .label(code.clone())
                .selected(*code == e.language)
                .on_click(send_to(&main, move || {
                    Msg::SettingsSetLanguage(wanted.clone())
                }))
        });

        v_flex()
            .id("settings")
            .test_support()
            .gap_3()
            .px_4()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(t("settings.title")),
            )
            .child(field("settings-batch", t("settings.batch"), &self.batch))
            .child(field(
                "settings-auto-scan",
                t("settings.auto_scan"),
                &self.auto_scan,
            ))
            .child({
                let view = main.clone();
                Checkbox::new("settings-show-details")
                    .label(t("settings.show_details"))
                    .checked(e.show_key_details)
                    .on_click(move |on, window, cx| {
                        let on = *on;
                        view.update(cx, |this, cx| {
                            this.dispatch(Msg::SettingsSetShowDetails(on), window, cx)
                        });
                    })
            })
            .child(
                v_flex()
                    .gap_1()
                    .child(div().text_sm().child(t("settings.language")))
                    .child(h_flex().gap_1().flex_wrap().children(languages)),
            )
            .child(
                h_flex().child(
                    Button::new("settings-export")
                        .compact()
                        .ghost()
                        .label(t("settings.export_template"))
                        .on_click(send_to(&main, || Msg::ExportLanguageTemplate)),
                ),
            )
            .children(error_list("settings-errors", errors, cx))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("settings-cancel")
                            .label(t("btn.cancel"))
                            .on_click(send_to(&main, || Msg::SettingsCancel)),
                    )
                    .child(
                        Button::new("settings-save")
                            .primary()
                            .label(t("btn.save"))
                            .disabled(!e.can_save(&app.settings))
                            .on_click(send_to(&main, || Msg::SettingsSave)),
                    ),
            )
            .into_any_element()
    }
}
