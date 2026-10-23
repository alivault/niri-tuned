#version 100

// The blur behind a wobbly window, through the window's mesh: positions from 0 to 1 across the
// element, the mask (the effect's shape, bent with the window) and the vertex's place in the
// xray backdrop (fixed to the screen).
uniform mat3 matrix;

attribute vec2 position;
attribute vec2 mask_coords;
attribute vec2 backdrop_coords;

varying vec2 v_mask_coords;
varying vec2 v_backdrop_coords;

void main() {
    v_mask_coords = mask_coords;
    v_backdrop_coords = backdrop_coords;
    gl_Position = vec4(matrix * vec3(position, 1.0), 1.0);
}
