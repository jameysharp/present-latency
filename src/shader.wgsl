struct Stipple {
    @location(0) offset: vec2i,
}

struct Vertex {
    @builtin(position) position: vec4f,
    @location(0) @interpolate(flat) stipple_offset: vec2i,
}

@vertex
fn vertex(@builtin(vertex_index) vertex_index: u32, stipple: Stipple) -> Vertex {
    var out: Vertex;
    let corner = vec2f(
        // x is -1, 1, -1, 1 (left, then right)
        select(-1.0, 1.0, (vertex_index & 1) != 0),
        // y is 1, 1, -1, -1 (top, then bottom)
        select(1.0, -1.0, (vertex_index & 2) != 0),
    );
    out.position = vec4f(corner, 0, 1);
    out.stipple_offset = stipple.offset;
    return out;
}

@fragment
fn fragment(in: Vertex) -> @location(0) vec4f {
    let position = vec2i(in.position.xy) - in.stipple_offset;
    let wrapped = vec2u(position) & vec2u(3);
    // https://matttproud.com/blog/posts/x-window-system-boot-stipple.html
    const ROOT_WEAVE: u32 = 0x0213;
    let white_x = (ROOT_WEAVE >> (wrapped.y * 4)) & 3;
    if wrapped.x == white_x {
        return vec4f(1.0, 1.0, 1.0, 1.0);
    } else {
        return vec4f(0.0, 0.0, 0.0, 1.0);
    }
}
