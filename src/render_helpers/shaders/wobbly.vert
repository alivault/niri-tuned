#version 100

// A wobbly window's mesh: positions from 0 to 1 across the element.
uniform mat3 matrix;

attribute vec2 position;
attribute vec2 tex_coords;

varying vec2 v_tex_coords;

void main() {
    v_tex_coords = tex_coords;
    gl_Position = vec4(matrix * vec3(position, 1.0), 1.0);
}
