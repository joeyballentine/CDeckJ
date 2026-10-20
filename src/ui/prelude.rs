//! Names nearly every UI file uses, so each can start with `use crate::ui::prelude::*;`.

pub(crate) use std::time::{Duration, Instant};

pub(crate) use egui::text::{LayoutJob, TextWrapping};
pub(crate) use egui::{
    pos2, vec2, Align2, Color32, CornerRadius, FontId, Id, Key, Painter, Pos2, Rect, Sense, Shape, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo,
};

pub(crate) use crate::engine::{Cmd, DeckView, EngineView, MixParam, DECKS};
pub(crate) use crate::midi::Ctl as MidiCtl;

pub(crate) use super::deckinfo::{bar_position, bars_beats, key_label, next_cue};
pub(crate) use super::icons;
pub(crate) use super::theme::{self as t, caption, cond, mono, semi, sz};
pub(crate) use super::touch::Touches;
pub(crate) use crate::ui::views::View;
pub(crate) use super::widgets::*;
