//! The detail screen: a fixed header (the way back and the state; the slug
//! and the kind; the title), then one scrolling column with the polish state,
//! the Markdown body and the review sections, then the error line and the
//! actions, fixed at the bottom.
//!
//! The kind and the title can be changed while the intent is a draft that is
//! not being polished, or is complete ([`header_editable`]). The kind is a
//! menu of the five kinds the create screen offers; the title is a button
//! that becomes a one-line input: Enter saves a changed title, Escape (or
//! Enter on a blank or unchanged one) closes it. Neither changes anything on
//! screen before the daemon's copy reaches the model; the input closes when
//! the daemon answers, and stays (with the error line) when it refuses.
//!
//! The body is the intent's current behavior, or its seed text while that is
//! blank ([`body_text`]), drawn read-only as Markdown. While the daemon
//! polishes the draft the screen says which pass it is on and draws no
//! sections or actions; when a pass failed it says why and offers "Try
//! again".

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Escape, Input, InputState};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Disableable as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, rems, AnyElement, App, AppContext as _, Context, ElementId, Entity, Focusable as _,
    FontWeight, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, WeakEntity, Window,
};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{Intent, IntentKind, IntentState, Patch, Phase};

use super::{Busy, IntentsPane, Paint, Screen, TITLE};
use crate::intent_vm::{
    body_text, header_editable, intent_state_label, kind_label, polishing_text, KIND_CHOICES,
};

/// Element id of the button back to the list ("Intents").
pub const DETAIL_BACK_ID: &str = "intent-back";
/// Element id of the state label ("Review", "Sent" and so on).
pub const DETAIL_STATE_ID: &str = "intent-state";
/// Element id of the slug.
pub const DETAIL_SLUG_ID: &str = "intent-slug";
/// Element id of the kind: the kind menu's button, or a fixed label.
pub const DETAIL_KIND_ID: &str = "intent-kind";
/// Element id of the title: a button that opens the title input, or a
/// fixed label.
pub const DETAIL_TITLE_ID: &str = "intent-title";
/// Element id of the title input.
pub const DETAIL_TITLE_INPUT_ID: &str = "intent-title-input";
/// Element id of the column that scrolls under the header.
pub const DETAIL_SCROLL_ID: &str = "intent-scroll";
/// Element id of the frame around the Markdown body.
pub const DETAIL_BODY_ID: &str = "intent-body";
/// Element id of "Polishing… (pass n of 2)".
pub const POLISHING_ID: &str = "intent-polishing";
/// Element id of "Polish failed: …" (a status).
pub const POLISH_FAILED_ID: &str = "intent-polish-failed";
/// Element id of the Try again button.
pub const RETRY_ID: &str = "intent-retry";

const RETRY_LABEL: &str = "Try again";
const TITLE_INPUT_LABEL: &str = "Title";

/// Element id of `kind`'s entry in the kind menu.
pub fn kind_item_id(kind: IntentKind) -> ElementId {
    ElementId::Name(format!("intent-kind-item-{}", kind.wire_name()).into())
}

/// The title input: one line, Enter saves.
pub(super) fn new_title_input(window: &mut Window, cx: &mut App) -> Entity<InputState> {
    cx.new(|cx| InputState::new(window, cx).placeholder(TITLE_INPUT_LABEL))
}

/// Whether the daemon is polishing the draft.
fn polishing(intent: &Intent) -> bool {
    intent.state == IntentState::Draft && matches!(intent.phase, Phase::Polishing { .. })
}

impl IntentsPane {
    /// The Markdown the open detail hands its body view: the current
    /// behavior, or the seed text while that is blank. `None` on another
    /// screen, or before the intent reaches the model. (A test cannot read
    /// the text a Markdown view draws, so it reads this.)
    pub fn body_source(&self, cx: &App) -> Option<String> {
        let Screen::Detail(id) = self.screen else {
            return None;
        };
        let model = self.state.read(cx).model();
        model.intent(id).map(|intent| body_text(intent).to_owned())
    }

    /// The detail's parts below the body, in order: the review sections
    /// ([`super::sections`]). Empty while the phase is Polishing.
    pub(crate) fn detail_sections(
        &self,
        intent: &Intent,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> Vec<AnyElement> {
        if polishing(intent) {
            return Vec::new();
        }
        self.review_sections(intent, paint, cx)
            .into_iter()
            .collect()
    }

    /// The bar fixed at the bottom of the detail; Task 10 fills it. `None`
    /// while the phase is Polishing.
    pub(crate) fn detail_actions(
        &self,
        intent: &Intent,
        _: &Paint,
        _: &Context<Self>,
    ) -> Option<AnyElement> {
        if polishing(intent) {
            return None;
        }
        // Task 10 draws the bar here.
        None
    }

    pub(super) fn detail_screen(
        &self,
        id: IntentId,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> AnyElement {
        let Some(intent) = self.state.read(cx).model().intent(id) else {
            // A created intent whose copy has not arrived yet: the way back.
            return v_flex()
                .size_full()
                .child(
                    h_flex()
                        .flex_none()
                        .px_1()
                        .pt_2()
                        .child(back_button(paint, cx)),
                )
                .into_any_element();
        };
        v_flex()
            .size_full()
            .child(self.detail_header(intent, paint, cx))
            .child(
                v_flex()
                    .id(DETAIL_SCROLL_ID)
                    .test_support()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.detail_scroll)
                    .px_3()
                    .pb_3()
                    .gap_3()
                    .children(self.polish_state(intent, paint, cx))
                    .child(body(intent, paint))
                    .children(self.detail_sections(intent, paint, cx)),
            )
            .children(self.error_line(paint))
            .children(self.detail_actions(intent, paint, cx))
            .into_any_element()
    }

    fn detail_header(
        &self,
        intent: &Intent,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let editable = header_editable(intent);
        v_flex()
            .flex_none()
            .w_full()
            .pt_2()
            .pb_2()
            .gap_1p5()
            .child(
                h_flex()
                    .w_full()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .pl_1()
                    .pr_3()
                    .child(back_button(paint, cx))
                    .child(
                        Label::new(intent_state_label(intent))
                            .id(DETAIL_STATE_ID)
                            .flex_none()
                            .text_xs()
                            .text_color(paint.muted_foreground),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .px_3()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Tag::secondary()
                            .small()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(
                                Label::new(intent.slug.clone())
                                    .id(DETAIL_SLUG_ID)
                                    .min_w_0()
                                    .line_height(rems(1.))
                                    .text_color(paint.token("secondary-foreground"))
                                    .truncate(),
                            ),
                    )
                    .child(self.kind_chip(intent, editable, cx)),
            )
            .child(
                div()
                    .w_full()
                    .px_2()
                    .child(self.title_element(intent, editable, paint, cx)),
            )
    }

    /// With `editable`, a small button that opens the kind menu; otherwise
    /// a tag.
    fn kind_chip(&self, intent: &Intent, editable: bool, cx: &Context<Self>) -> AnyElement {
        let current = intent.content.kind;
        if !editable {
            return Tag::secondary()
                .outline()
                .small()
                .flex_none()
                .child(
                    Label::new(kind_label(current))
                        .id(DETAIL_KIND_ID)
                        .line_height(rems(1.)),
                )
                .into_any_element();
        }
        let (id, pane) = (intent.id, cx.entity().downgrade());
        Button::new(DETAIL_KIND_ID)
            .label(kind_label(current))
            .outline()
            .xsmall()
            .flex_none()
            .dropdown_caret(true)
            .disabled(self.busy.is_some())
            .loading(self.busy == Some(Busy::Kind))
            .dropdown_menu(move |menu, _, _| kind_menu(menu, id, current, &pane))
            .into_any_element()
    }

    /// With `editable`, a button that opens the title input (or the input,
    /// while open); otherwise a label.
    fn title_element(
        &self,
        intent: &Intent,
        editable: bool,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> AnyElement {
        let title = intent.content.title.clone();
        if !editable {
            return Label::new(title)
                .id(DETAIL_TITLE_ID)
                .px_1()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(paint.foreground)
                .into_any_element();
        }
        if self.editing_title == Some(intent.id) {
            let saving = self.busy == Some(Busy::Title);
            let muted = paint.muted_foreground;
            // The input passes Escape on; it ends here.
            return div()
                .w_full()
                .on_action(
                    cx.listener(|this, _: &Escape, window, cx| this.close_title_edit(window, cx)),
                )
                .child(
                    Input::new(&self.title_input)
                        .id(DETAIL_TITLE_INPUT_ID)
                        .aria_label(TITLE_INPUT_LABEL)
                        .small()
                        .when(saving, |input| {
                            input.suffix(Spinner::new().xsmall().color(muted))
                        }),
                )
                .into_any_element();
        }
        let id = intent.id;
        let (hover, ring) = (paint.hover, paint.ring);
        div()
            .id(DETAIL_TITLE_ID)
            .role(Role::Button)
            .test_support()
            .track_focus(&self.title_focus)
            .aria_label(format!("Edit title: {title}"))
            .w_full()
            .px_1()
            .rounded_sm()
            .border_1()
            .border_color(paint.surface)
            .cursor_pointer()
            .text_sm()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(paint.foreground)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(ring))
            .on_click(cx.listener(move |this, _, window, cx| this.edit_title(id, window, cx)))
            .child(title)
            .into_any_element()
    }

    /// "Polishing… (pass n of 2)" while the daemon polishes the draft; why a
    /// pass failed, and Try again, after it failed.
    fn polish_state(
        &self,
        intent: &Intent,
        paint: &Paint,
        cx: &Context<Self>,
    ) -> Option<AnyElement> {
        if intent.state != IntentState::Draft {
            return None;
        }
        match &intent.phase {
            Phase::Polishing { pass } => Some(
                h_flex()
                    .w_full()
                    .items_center()
                    .gap_2()
                    .child(Spinner::new().small().color(paint.muted_foreground))
                    .child(
                        Label::new(polishing_text(*pass))
                            .id(POLISHING_ID)
                            .text_sm()
                            .text_color(paint.muted_foreground),
                    )
                    .into_any_element(),
            ),
            Phase::Failed { message, .. } => {
                let text = format!("Polish failed: {message}");
                let id = intent.id;
                Some(
                    v_flex()
                        .w_full()
                        .gap_2()
                        .p_2()
                        .rounded_md()
                        .border_1()
                        .border_color(paint.destructive)
                        .child(
                            div()
                                .id(POLISH_FAILED_ID)
                                .role(Role::Status)
                                .test_support()
                                .aria_label(text.clone())
                                .text_sm()
                                .text_color(paint.destructive)
                                .child(text),
                        )
                        .child(
                            h_flex().child(
                                Button::new(RETRY_ID)
                                    .label(RETRY_LABEL)
                                    .outline()
                                    .small()
                                    .disabled(self.busy.is_some())
                                    .loading(self.busy == Some(Busy::Retry))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.retry_polish(id, window, cx)
                                    })),
                            ),
                        )
                        .into_any_element(),
                )
            }
            Phase::Questions | Phase::Review => None,
        }
    }

    /// Asks the daemon to change the kind to `kind`; nothing when it already
    /// is.
    fn patch_kind(
        &mut self,
        id: IntentId,
        kind: IntentKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let current = self
            .state
            .read(cx)
            .model()
            .intent(id)
            .map(|i| i.content.kind);
        if current.is_none_or(|current| current == kind) {
            return;
        }
        tracing::debug!(%id, ?kind, "changing an intent's kind");
        let patch = Patch {
            kind: Some(kind),
            ..Default::default()
        };
        self.command(
            Busy::Kind,
            move |commands| commands.update_intent(id, patch),
            |_, _, _, _| {},
            window,
            cx,
        );
    }

    /// Opens the title input, holding the title, with the focus.
    fn edit_title(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        let title = self
            .state
            .read(cx)
            .model()
            .intent(id)
            .filter(|intent| header_editable(intent))
            .map(|intent| intent.content.title.clone());
        let Some(title) = title else {
            return;
        };
        self.editing_title = Some(id);
        self.title_input.update(cx, |input, cx| {
            input.set_value(title, window, cx);
            input.focus(window, cx);
        });
        cx.notify();
    }

    /// Enter in the title input: a changed, non-blank title (trimmed) is
    /// sent, and the input closes when the daemon answers; a blank or
    /// unchanged one just closes it. Nothing while a command is in flight.
    pub(super) fn save_title(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.editing_title else {
            return;
        };
        if self.busy.is_some() {
            return;
        }
        let text = self.title_input.read(cx).value().trim().to_owned();
        let unchanged = self
            .state
            .read(cx)
            .model()
            .intent(id)
            .is_none_or(|intent| intent.content.title == text);
        if text.is_empty() || unchanged {
            self.close_title_edit(window, cx);
            return;
        }
        tracing::debug!(%id, "renaming an intent");
        let patch = Patch {
            title: Some(text),
            ..Default::default()
        };
        self.command(
            Busy::Title,
            move |commands| commands.update_intent(id, patch),
            move |this, _, window, cx| {
                if this.editing_title == Some(id) {
                    this.close_title_edit(window, cx);
                }
            },
            window,
            cx,
        );
    }

    /// Closes the title input. A keyboard user stays on the title.
    fn close_title_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_title.take().is_none() {
            return;
        }
        if self.title_input.focus_handle(cx).is_focused(window) {
            self.title_focus.focus(window, cx);
        }
        cx.notify();
    }

    fn retry_polish(&mut self, id: IntentId, window: &mut Window, cx: &mut Context<Self>) {
        tracing::debug!(%id, "retrying a failed polish");
        self.command(
            Busy::Retry,
            move |commands| commands.retry_polish(id),
            |_, _, _, _| {},
            window,
            cx,
        );
    }
}

/// The button back to the list.
fn back_button(paint: &Paint, cx: &Context<IntentsPane>) -> impl IntoElement {
    Button::new(DETAIL_BACK_ID)
        .icon(Icon::new(IconName::ArrowLeft).text_color(paint.foreground))
        .label(TITLE)
        .ghost()
        .small()
        .on_click(cx.listener(|this, _, window, cx| this.open_screen(Screen::List, window, cx)))
}

/// The body as Markdown, read-only, in a frame the tests can find.
fn body(intent: &Intent, paint: &Paint) -> impl IntoElement {
    div()
        .id(DETAIL_BODY_ID)
        .test_support()
        .w_full()
        .min_w_0()
        .text_sm()
        .child(
            TextView::markdown(DETAIL_BODY_ID, body_text(intent).to_owned())
                .selectable(true)
                .text_color(paint.foreground),
        )
}

/// The kind menu: the five kinds the create screen offers, the current one
/// checked (none for an intent of the old kind Task).
fn kind_menu(
    mut menu: PopupMenu,
    intent: IntentId,
    current: IntentKind,
    pane: &WeakEntity<IntentsPane>,
) -> PopupMenu {
    for choice in KIND_CHOICES {
        let (kind, title) = (choice.kind, choice.title);
        let pane = pane.clone();
        menu = menu.item(
            // gpui-component gives an element item's menu-item node no name
            // (`PopupMenuItem::a11y_label` is `None` for it), so the entry
            // carries its own named node.
            PopupMenuItem::element(move |_, _| Label::new(title).id(kind_item_id(kind)))
                .checked(kind == current)
                .on_click(move |_, window, cx| {
                    let _ = pane.update(cx, |pane, cx| pane.patch_kind(intent, kind, window, cx));
                }),
        );
    }
    menu
}
