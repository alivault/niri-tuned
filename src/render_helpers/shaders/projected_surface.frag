#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision highp float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// A window surface drawn in perspective (the recent-windows Flip 3D): the
// element is drawn over the box around its projected quad, and each pixel
// looks up where it falls on the window through the inverse homography.

// Texture coordinates to the destination box, 0..1.
uniform mat3 input_to_box;
// The destination box to window-logical coordinates (projective).
uniform mat3 box_to_window;
// One destination pixel, in box units.
uniform vec2 box_pixel;
// The surface in window-logical coordinates: x, y, width, height.
uniform vec4 surface_rect;
// Surface coordinates (0..1) back to texture coordinates.
uniform mat3 surface_to_input;
// Texels across the surface, along its own axes.
uniform vec2 surface_texels;
// Where the surface's own edges cut (surface coordinates); far outside where
// the window geometry's edge does, so that edge is antialiased instead.
uniform vec4 surface_cut;
// The window geometry (logical) and its corner radii.
uniform vec2 geo_size;
uniform vec4 corner_radius;

// Destination pixels per window-logical unit, for niri_rounding_alpha().
float niri_scale;

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

vec2 window_at(vec2 box) {
    vec3 p = box_to_window * vec3(box, 1.0);
    return p.xy / p.z;
}

void main() {
    vec2 box = (input_to_box * vec3(v_coords, 1.0)).xy;
    vec2 win = window_at(box);
    // How far one destination pixel reaches on the window, along x and y.
    vec2 win_dx = window_at(box + vec2(box_pixel.x, 0.0)) - win;
    vec2 win_dy = window_at(box + vec2(0.0, box_pixel.y)) - win;

    vec2 surf = (win - surface_rect.xy) / surface_rect.zw;
    if (surf.x < surface_cut.x || surf.y < surface_cut.y
            || surface_cut.z < surf.x || surface_cut.w < surf.y) {
        gl_FragColor = vec4(0.0);
        return;
    }

    // Integrate the pixel's footprint on the surface: 1..6 bilinear samples
    // per axis, so far-away (small) windows don't alias. Premultiplied colors
    // averaged, alpha included.
    vec2 surf_dx = win_dx / surface_rect.zw;
    vec2 surf_dy = win_dy / surface_rect.zw;
    vec2 count = clamp(ceil(vec2(length(surf_dx * surface_texels),
                                 length(surf_dy * surface_texels))), 1.0, 6.0);
    vec2 inset = 0.5 / max(surface_texels, vec2(1.0));
    vec4 color = vec4(0.0);
    for (int y = 0; y < 6; y++) {
        if (float(y) >= count.y) break;
        for (int x = 0; x < 6; x++) {
            if (float(x) >= count.x) break;
            vec2 offset = (vec2(float(x), float(y)) + 0.5) / count - 0.5;
            vec2 s = clamp(surf + offset.x * surf_dx + offset.y * surf_dy, inset, 1.0 - inset);
            color += texture2D(tex, (surface_to_input * vec3(s, 1.0)).xy);
        }
    }
    color /= count.x * count.y;
#if defined(NO_ALPHA)
    color.a = 1.0;
#endif

    // Coverage of the window geometry, one destination pixel soft: each edge's
    // distance in pixels is its distance on the window over how fast that
    // coordinate changes across the screen.
    float gx = max(length(vec2(win_dx.x, win_dy.x)), 1e-6);
    float gy = max(length(vec2(win_dx.y, win_dy.y)), 1e-6);
    float coverage = clamp(win.x / gx + 0.5, 0.0, 1.0)
                   * clamp((geo_size.x - win.x) / gx + 0.5, 0.0, 1.0)
                   * clamp(win.y / gy + 0.5, 0.0, 1.0)
                   * clamp((geo_size.y - win.y) / gy + 0.5, 0.0, 1.0);
    niri_scale = inversesqrt(max(abs(win_dx.x * win_dy.y - win_dx.y * win_dy.x), 1e-12));
    coverage *= niri_rounding_alpha(clamp(win, vec2(0.0), geo_size), geo_size, corner_radius);

    color *= coverage * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
