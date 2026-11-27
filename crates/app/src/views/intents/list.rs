//! The list screen: the heading with the filter and New intent buttons, the
//! error line, then one card per intent the filter shows, newest change
//! first, or what the list says instead ([`list_empty_text`]).
//!
//! A card is a button: a click, or Enter or Space on the focused card, opens
//! its detail. It shows a bar in the agent's color (a Sent intent whose agent
//! is open; else the card's border color), the status dot, the slug, the
//! title, the meta line and the body's first heading and line. Every word
//! comes from [`intent_cards`]; the views derive no text.

use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::label::Label;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{h_flex, v_flex, ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    div, px, rems, Anchor, Context, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _,
    WeakEntity,
};

use super::{
    filter_item_id, intent_card_id, IntentsPane, Paint, Screen, CARD_ACCENT_ID, CARD_DOT_ID,
    CARD_HEADING_ID, CARD_LINE_ID, CARD_META_ID, CARD_SLUG_ID, CARD_TITLE_ID, INTENTS_EMPTY_ID,
    INTENTS_FILTER_ID, INTENTS_NEW_ID, INTENTS_TITLE_ID, INTENT_LIST_ID, TITLE,
};
use crate::intent_vm::{intent_cards, list_empty_text, IntentCardVm, IntentFilterChoice};
use crate::views::agents::status_dot;
use crate::views::pane_heading;

/// The filter button's tooltip and accessible name.
const FILTER_LABEL: &str = "Filter";
/// The filter menu's label.
const FILTER_MENU_LABEL: &str = "Show";
/// The New intent button's tooltip and accessible name.
const NEW_LABEL: &str = "New intent";

const ACCENT_BAR_WIDTH: f32 = 3.;
const CARD_DOT: f32 = 8.;

impl IntentsPane {
    pub(super) fn list_screen(
        &mut self,
        paint: &Paint,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (cards, empty) = {
            let state = self.state.read(cx);
            let model = state.model();
            (
                intent_cards(model, self.filter, state.now()),
                list_empty_text(model, self.filter),
            )
        };
        self.sync_card_focus(&cards, cx);
        v_flex()
            .size_full()
            .child(self.list_header(paint, cx))
            .children(self.error_line(paint))
            .child(match empty {
                // A `Label` is in the accessibility tree; plain text is not.
                Some(text) => Label::new(text)
                    .id(INTENTS_EMPTY_ID)
                    .px_3()
                    .text_xs()
                    .text_color(paint.muted_foreground)
                    .into_any_element(),
                None => v_flex()
                    .id(INTENT_LIST_ID)
                    .role(Role::List)
                    .test_support()
                    .aria_label(TITLE)
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .px_2()
                    .pb_2()
                    .gap_1p5()
                    .children(cards.into_iter().map(|card| self.card(card, paint, cx)))
                    .into_any_element(),
            })
    }

    fn set_filter(&mut self, filter: IntentFilterChoice, cx: &mut Context<Self>) {
        tracing::debug!(?filter, "intents filter");
        self.filter = filter;
        cx.notify();
    }

    /// Keeps one focus handle per card drawn.
    fn sync_card_focus(&mut self, cards: &[IntentCardVm], cx: &mut Context<Self>) {
        self.card_focus
            .retain(|id, _| cards.iter().any(|card| card.id == *id));
        for card in cards {
            self.card_focus
                .entry(card.id)
                .or_insert_with(|| cx.focus_handle().tab_stop(true));
        }
    }

    fn list_header(&self, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let pane = cx.entity().downgrade();
        let current = self.filter;
        h_flex()
            .flex_none()
            .w_full()
            .items_center()
            .justify_between()
            .gap_2()
            .pl_3()
            .pr_1()
            .py_2()
            .child(
                pane_heading(INTENTS_TITLE_ID, TITLE, paint.foreground)
                    .min_w_0()
                    .truncate(),
            )
            .child(
                h_flex()
                    .flex_none()
                    .items_center()
                    .gap_0p5()
                    .child(
                        Button::new(INTENTS_FILTER_ID)
                            .icon(Icon::new(IconName::ListFilter).text_color(paint.foreground))
                            .ghost()
                            .small()
                            .tooltip(FILTER_LABEL)
                            .accessibility_label(FILTER_LABEL)
                            // At the pane's right edge: the menu opens to the
                            // left.
                            .dropdown_menu_with_anchor(Anchor::TopRight, move |menu, _, _| {
                                filter_menu(menu, current, &pane)
                            }),
                    )
                    .child(
                        Button::new(INTENTS_NEW_ID)
                            .icon(Icon::new(IconName::Plus).text_color(paint.foreground))
                            .ghost()
                            .small()
                            .tooltip(NEW_LABEL)
                            .accessibility_label(NEW_LABEL)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_screen(Screen::Create, window, cx)
                            })),
                    ),
            )
    }

    fn card(&self, card: IntentCardVm, paint: &Paint, cx: &Context<Self>) -> impl IntoElement {
        let id = card.id;
        let (hover, ring) = (paint.hover, paint.ring);
        let bar = card
            .accent_hue
            .and_then(|hue| paint.accent(hue))
            .unwrap_or(paint.border);
        h_flex()
            .id(intent_card_id(id))
            .role(Role::Button)
            .test_support()
            .track_focus(&self.card_focus[&id])
            .aria_label(card.a11y_label)
            .flex_none()
            .w_full()
            .min_w_0()
            .gap_2()
            .pr_2()
            .border_1()
            .border_color(paint.border)
            .bg(paint.surface)
            .hover(move |style| style.bg(hover))
            .focus_visible(move |style| style.border_color(ring))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_screen(Screen::Detail(id), window, cx)
            }))
            .child(
                div()
                    .id(CARD_ACCENT_ID)
                    .test_support()
                    .flex_none()
                    .w(px(ACCENT_BAR_WIDTH))
                    // The card's height comes from its text, so stretch.
                    .self_stretch()
                    .bg(bar),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .py_1p5()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_1p5()
                            .items_center()
                            .child(
                                status_dot(card.dot, CARD_DOT, paint.token(card.dot.token))
                                    .id(CARD_DOT_ID)
                                    .test_support(),
                            )
                            .child(
                                Tag::secondary()
                                    .small()
                                    .min_w_0()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .child(
                                        Label::new(card.slug)
                                            .id(CARD_SLUG_ID)
                                            .min_w_0()
                                            .line_height(rems(1.))
                                            .text_color(paint.token("secondary-foreground"))
                                            .truncate(),
                                    ),
                            ),
                    )
                    .child(
                        Label::new(card.title)
                            .id(CARD_TITLE_ID)
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(paint.foreground)
                            .truncate(),
                    )
                    .child(
                        Label::new(card.meta)
                            .id(CARD_META_ID)
                            .text_xs()
                            .text_color(paint.muted_foreground)
                            .truncate(),
                    )
                    .when_some(card.preview.heading, |this, heading| {
                        this.child(
                            Label::new(heading)
                                .id(CARD_HEADING_ID)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(paint.foreground)
                                .truncate(),
                        )
                    })
                    .when_some(card.preview.line, |this, line| {
                        this.child(
                            Label::new(line)
                                .id(CARD_LINE_ID)
                                .text_xs()
                                .text_color(paint.muted_foreground)
                                .truncate(),
                        )
                    }),
            )
    }
}

/// The filter menu, labelled "Show": the five choices, the current one
/// checked.
fn filter_menu(
    mut menu: PopupMenu,
    current: IntentFilterChoice,
    pane: &WeakEntity<IntentsPane>,
) -> PopupMenu {
    menu = menu.label(FILTER_MENU_LABEL);
    for choice in IntentFilterChoice::ALL {
        let pane = pane.clone();
        menu = menu.item(
            // gpui-component gives an element item's menu-item node no name
            // (`PopupMenuItem::a11y_label` is `None` for it), so the entry
            // carries its own named node.
            PopupMenuItem::element(move |_, _| {
                Label::new(choice.label()).id(filter_item_id(choice))
            })
            .checked(choice == current)
            .on_click(move |_, _, cx| {
                let _ = pane.update(cx, |pane, cx| pane.set_filter(choice, cx));
            }),
        );
    }
    menu
}
