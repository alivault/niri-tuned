#version 100

precision highp float;

uniform sampler2D tex;
uniform float alpha;

varying vec2 v_tex_coords;

void main() {
    // The offscreen picture is premultiplied; outside it the border is transparent.
    gl_FragColor = texture2D(tex, v_tex_coords) * alpha;
}
