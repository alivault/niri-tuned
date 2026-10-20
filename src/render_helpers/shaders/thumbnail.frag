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

uniform float niri_scale;
uniform vec2 geo_size;
uniform vec4 corner_radius;
uniform mat3 input_to_geo;
uniform vec2 sample_area;
uniform vec2 sample_count;
uniform vec4 sample_bounds;

float niri_rounding_alpha(vec2 coords, vec2 size, vec4 corner_radius);

void main() {
    // Integrate the destination pixel's source footprint. Adaptive, bounded
    // quadrature uses 1..8 bilinear samples per axis, rather than one sample
    // skipping most source pixels. No offscreen, mipmap, or frame-callback loss.
    // Average premultiplied colors, including alpha, to avoid transparent fringes.
    vec4 color = vec4(0.0);
    for (int y = 0; y < 8; y++) {
        if (float(y) >= sample_count.y) break;
        for (int x = 0; x < 8; x++) {
            if (float(x) >= sample_count.x) break;
            vec2 offset = (vec2(float(x), float(y)) + 0.5) / sample_count - 0.5;
            vec2 uv = clamp(v_coords + offset * sample_area,
                            sample_bounds.xy, sample_bounds.zw);
            color += texture2D(tex, uv);
        }
    }
    color /= sample_count.x * sample_count.y;
#if defined(NO_ALPHA)
    color.a = 1.0;
#endif

    vec2 coords_geo = (input_to_geo * vec3(v_coords, 1.0)).xy;
    if (coords_geo.x < 0.0 || 1.0 < coords_geo.x || coords_geo.y < 0.0 || 1.0 < coords_geo.y) {
        color = vec4(0.0);
    } else {
        // niri_scale is the final thumbnail scale, so the coverage transition
        // stays one destination pixel wide instead of shrinking into a jaggy edge.
        color *= niri_rounding_alpha(coords_geo * geo_size, geo_size, corner_radius);
    }
    color *= alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
