//! The canvas: what a painting plugin describes, and the core paints.
//!
//! A program cannot touch the frame — no GPU, no window, no draw list — so the picture crosses the
//! wire as **primitives**: rectangles, lines of text and one level picture, in the plugin's own
//! slot-local space. The core translates them into its slot, clips them, caps how many there may
//! be and owns every other decision.
//!
//! Colours are **theme tokens by name**, never values: `accent`, `rule`, `text_faint`. A skin
//! switch moves the plugin's ink without the plugin knowing, and a program cannot introduce a
//! colour the theme audit has not measured — the same invariant the in-frame plugins get from
//! `ThemeView`, enforced here at the only door a third-party colour could come through.

use crate::json::Value;

/// One primitive. Geometry is in the slot's logical units, the same space `FrameContext` uses.
#[derive(Clone, Debug, PartialEq)]
pub enum Op {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        /// Corner radius, clamped by the core to half the shorter side.
        radius: f32,
        color: &'static str,
    },
    Text {
        text: String,
        x: f32,
        y: f32,
        /// The font size in logical units.
        size: f32,
        color: &'static str,
    },
    /// A cached level picture: `width × height` bytes of 0..=255, coloured by four tokens from
    /// the quiet floor to the peak. The core rasterises it once per frame into its own texture,
    /// which is why the bytes are base64 and not a list of numbers.
    Waterfall {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        width: u32,
        height: u32,
        levels: Vec<u8>,
        ramp: [&'static str; 4],
    },
}

/// One frame's drawing, in paint order.
#[derive(Default, Clone, Debug, PartialEq)]
pub struct Canvas {
    ops: Vec<Op>,
}

impl Canvas {
    pub fn new() -> Canvas {
        Canvas::default()
    }

    pub fn rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: &'static str,
    ) -> &mut Canvas {
        self.ops.push(Op::Rect {
            x,
            y,
            w,
            h,
            radius,
            color,
        });
        self
    }

    pub fn text(
        &mut self,
        text: impl Into<String>,
        x: f32,
        y: f32,
        size: f32,
        color: &'static str,
    ) -> &mut Canvas {
        self.ops.push(Op::Text {
            text: text.into(),
            x,
            y,
            size,
            color,
        });
        self
    }

    #[allow(clippy::too_many_arguments)]
    pub fn waterfall(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        width: u32,
        height: u32,
        levels: Vec<u8>,
        ramp: [&'static str; 4],
    ) -> &mut Canvas {
        self.ops.push(Op::Waterfall {
            x,
            y,
            w,
            h,
            width,
            height,
            levels,
            ramp,
        });
        self
    }

    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// The `draw` message: one object, ops in paint order.
    pub(crate) fn to_value(&self) -> Value {
        let ops: Vec<Value> = self.ops.iter().map(op_value).collect();
        obj(vec![
            ("type", text("draw")),
            ("ops", Value::Arr(ops)),
        ])
    }
}

fn op_value(op: &Op) -> Value {
    match op {
        Op::Rect {
            x,
            y,
            w,
            h,
            radius,
            color,
        } => obj(vec![
            ("op", text("rect")),
            ("x", num(*x)),
            ("y", num(*y)),
            ("w", num(*w)),
            ("h", num(*h)),
            ("radius", num(*radius)),
            ("color", text(color)),
        ]),
        Op::Text {
            text: body,
            x,
            y,
            size,
            color,
        } => obj(vec![
            ("op", text("text")),
            ("text", text(body)),
            ("x", num(*x)),
            ("y", num(*y)),
            ("size", num(*size)),
            ("color", text(color)),
        ]),
        Op::Waterfall {
            x,
            y,
            w,
            h,
            width,
            height,
            levels,
            ramp,
        } => obj(vec![
            ("op", text("waterfall")),
            ("x", num(*x)),
            ("y", num(*y)),
            ("w", num(*w)),
            ("h", num(*h)),
            ("width", num(*width as f32)),
            ("height", num(*height as f32)),
            ("levels", text(&crate::b64::encode(levels))),
            (
                "ramp",
                Value::Arr(ramp.iter().map(|token| text(token)).collect()),
            ),
        ]),
    }
}

fn obj(members: Vec<(&str, Value)>) -> Value {
    Value::Obj(
        members
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn text(value: &str) -> Value {
    Value::Str(value.to_string())
}

fn num(value: f32) -> Value {
    Value::Num(f64::from(value))
}

#[cfg(test)]
mod tests {
    use super::Canvas;
    use crate::json;

    fn parse(canvas: &Canvas) -> json::Value {
        json::parse(&canvas.to_value().to_json()).expect("the canvas is valid JSON")
    }

    #[test]
    fn a_rect_names_its_token_and_its_geometry() {
        let mut canvas = Canvas::new();
        canvas.rect(1.0, 2.0, 3.0, 4.0, 0.5, "rule");
        let root = parse(&canvas);
        assert_eq!(root.get("type").and_then(|v| v.as_str()), Some("draw"));
        let ops = root.get("ops").and_then(|v| v.as_arr()).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].get("op").and_then(|v| v.as_str()), Some("rect"));
        assert_eq!(ops[0].get("radius").and_then(|v| v.as_f64()), Some(0.5));
        assert_eq!(ops[0].get("color").and_then(|v| v.as_str()), Some("rule"));
    }

    #[test]
    fn a_text_op_carries_its_words() {
        let mut canvas = Canvas::new();
        canvas.text("20k", 0.0, 8.0, 8.0, "text_faint");
        let root = parse(&canvas);
        let ops = root.get("ops").and_then(|v| v.as_arr()).unwrap();
        assert_eq!(ops[0].get("text").and_then(|v| v.as_str()), Some("20k"));
        assert_eq!(ops[0].get("size").and_then(|v| v.as_f64()), Some(8.0));
    }

    #[test]
    fn a_waterfall_op_is_its_levels_and_its_ramp() {
        let levels: Vec<u8> = (0..=255).collect();
        let mut canvas = Canvas::new();
        canvas.waterfall(
            40.0,
            0.0,
            210.0,
            190.0,
            90,
            200,
            levels,
            ["panel_inset", "accent2", "accent", "text"],
        );
        let root = parse(&canvas);
        let ops = root.get("ops").and_then(|v| v.as_arr()).unwrap();
        assert_eq!(ops[0].get("width").and_then(|v| v.as_f64()), Some(90.0));
        let ramp = ops[0].get("ramp").and_then(|v| v.as_arr()).unwrap();
        assert_eq!(ramp.len(), 4);
        assert_eq!(ramp[2].as_str(), Some("accent"));
        // The picture is bytes on the wire, not a list of numbers.
        let data = ops[0].get("levels").and_then(|v| v.as_str()).unwrap();
        assert_eq!(crate::b64::decode(data).map(|bytes| bytes.len()), Some(256));
    }

    #[test]
    fn the_ops_keep_their_paint_order() {
        let mut canvas = Canvas::new();
        canvas.rect(0.0, 0.0, 1.0, 1.0, 0.0, "rule");
        canvas.text("a", 0.0, 0.0, 8.0, "text");
        canvas.rect(0.0, 0.0, 2.0, 2.0, 0.0, "accent");
        assert_eq!(canvas.len(), 3);
        let root = parse(&canvas);
        let ops = root.get("ops").and_then(|v| v.as_arr()).unwrap();
        let kinds: Vec<&str> = ops
            .iter()
            .filter_map(|op| op.get("op").and_then(|v| v.as_str()))
            .collect();
        assert_eq!(kinds, ["rect", "text", "rect"]);
        assert_eq!(ops[2].get("w").and_then(|v| v.as_f64()), Some(2.0));
    }
}
