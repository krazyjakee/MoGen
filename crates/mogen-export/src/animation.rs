use serde_json::{json, Value};

use mogen_core::{Clip, Interpolation, TrackProperty};

use crate::accessor::{push_times, push_track_values};
use crate::{Accessor, BufferView};

pub(crate) fn emit_animation(
    clip: &Clip,
    bin: &mut Vec<u8>,
    views: &mut Vec<BufferView>,
    accessors: &mut Vec<Accessor>,
) -> Value {
    let mut samplers: Vec<Value> = Vec::with_capacity(clip.tracks.len());
    let mut channels: Vec<Value> = Vec::with_capacity(clip.tracks.len());

    for track in &clip.tracks {
        let input_acc = push_times(bin, views, accessors, &track.times);
        let output_acc = push_track_values(bin, views, accessors, track);
        let interp = match track.interpolation {
            Interpolation::Linear => "LINEAR",
            Interpolation::Step => "STEP",
        };
        let sampler_idx = samplers.len();
        samplers.push(json!({
            "input": input_acc,
            "output": output_acc,
            "interpolation": interp,
        }));
        let path = match track.property {
            TrackProperty::Translation => "translation",
            TrackProperty::Rotation => "rotation",
            TrackProperty::Scale => "scale",
        };
        channels.push(json!({
            "sampler": sampler_idx,
            "target": { "node": track.node.0, "path": path },
        }));
    }

    json!({
        "name": clip.name,
        "channels": channels,
        "samplers": samplers,
    })
}
