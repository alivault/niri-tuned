#version 100

precision highp float;

uniform sampler2D mask;
uniform sampler2D tex;
uniform float alpha;

// 1: `tex` is the framebuffer copied from `uv_rect` (framebuffer pixels, as gl_FragCoord);
// 0: `tex` is an xray buffer at `uv_rect` in backdrop coordinates.
uniform float live;
uniform vec4 uv_rect;
// Draw nothing outside `uv_rect` (an xray workspace).
uniform float clip;

varying vec2 v_mask_coords;
varying vec2 v_backdrop_coords;

vec4 postprocess(vec4 color);

void main() {
    // The shape bends with the window; what shows through it stays where it is on screen.
    float shape = texture2D(mask, v_mask_coords).a;

    vec2 at = live > 0.5 ? gl_FragCoord.xy : v_backdrop_coords;
    vec2 uv = (at - uv_rect.xy) / uv_rect.zw;
    if (clip > 0.5 && (uv.x < 0.0 || uv.y < 0.0 || 1.0 < uv.x || 1.0 < uv.y)) {
        discard;
    }

    gl_FragColor = postprocess(texture2D(tex, uv)) * (shape * alpha);
}
