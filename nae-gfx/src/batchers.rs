//TODO https://www.gamedev.net/forums/topic/613184-what-is-the-vertex-limit-number-of-gldrawarrays/

// https://github.com/rustwasm/wasm-bindgen/issues/1389
// WASM32 uses vec because the initial memory is too low for a big array
type VERTICES = Vec<f32>;
type INDICES = Vec<u32>;

use crate::{
    matrix4_mul_vector4, DrawData, Graphics, IndexBuffer, Matrix4, Pipeline, Shader, Uniform,
    VertexAttr, VertexBuffer, VertexFormat,
};
use nae_core::{
    log, BaseGfx, BasePipeline, BlendMode, Color, DrawUsage, GraphicsAPI, PipelineOptions,
};

//https://webglfundamentals.org/webgl/lessons/webgl-indexed-vertices.html
#[inline]
fn max_vertices(gfx: &Graphics) -> usize {
    match gfx.api() {
        GraphicsAPI::WebGl => std::u16::MAX as usize,
        _ => std::u32::MAX as usize,
    }
}

#[inline]
fn batch_vertices(offset: usize) -> usize {
    let offset = offset as f32;
    let max = std::u16::MAX as usize;
    let size = {
        let mut n = max;
        while n > 0 {
            let nf = n as f32;
            if nf % offset == 0.0 && nf % 3.0 == 0.0 {
                break;
            }
            n -= 1;
        }
        n
    };

    size
}

/// Image batcher
pub(crate) struct ImageBatcher {
    pipeline: Pipeline,
    vbo: VertexBuffer,
    ibo: IndexBuffer,
    vertices: VERTICES,
    indices: INDICES,
    matrix_loc: Uniform,
    texture_matrix_loc: Uniform,
    texture_loc: Uniform,
    index: usize,
    max_vertices: usize,
    batch_size: usize,
}

impl ImageBatcher {
    pub fn new(gfx: &mut Graphics) -> Result<Self, String> {
        let shader = Shader::new(gfx, Shader::IMAGE_VERTEX, Shader::IMAGE_FRAG)?;
        let pipeline = Pipeline::new(
            gfx,
            &shader,
            PipelineOptions {
                color_blend: Some(BlendMode::NORMAL),
                ..Default::default()
            },
        );

        let matrix_loc = pipeline.uniform_location("u_matrix");
        let texture_matrix_loc = pipeline.uniform_location("u_tex_matrix");
        let texture_loc = pipeline.uniform_location("u_texture");

        let vertex_buffer = VertexBuffer::new(
            gfx,
            &[
                VertexAttr::new(0, VertexFormat::Float3),
                VertexAttr::new(1, VertexFormat::Float4),
                VertexAttr::new(2, VertexFormat::Float2),
            ],
            DrawUsage::Dynamic,
        )?;

        let index_buffer = IndexBuffer::new(gfx, DrawUsage::Dynamic)?;

        let max_vertices = max_vertices(gfx);
        let batch_size = batch_vertices(vertex_buffer.offset());

        let vertices = vec![0.0; batch_size];
        let indices = vec![0; batch_size / vertex_buffer.offset()];

        Ok(Self {
            pipeline,
            vbo: vertex_buffer,
            ibo: index_buffer,
            matrix_loc,
            texture_loc,
            texture_matrix_loc,
            vertices,
            indices,
            index: 0,
            max_vertices,
            batch_size,
        })
    }
}

/// Color batcher
pub(crate) struct ColorBatcher {
    pipeline: Pipeline,
    vbo: VertexBuffer,
    ibo: IndexBuffer,
    vertices: VERTICES,
    indices: INDICES,
    matrix_loc: Uniform,
    index: usize,
    max_vertices: usize,
    batch_size: usize,
}

impl ColorBatcher {
    pub fn new(gfx: &mut Graphics) -> Result<Self, String> {
        let shader = Shader::new(gfx, Shader::COLOR_VERTEX, Shader::COLOR_FRAG)?;
        let pipeline = Pipeline::new(
            gfx,
            &shader,
            PipelineOptions {
                color_blend: Some(BlendMode::NORMAL),
                ..Default::default()
            },
        );

        let matrix_loc = pipeline.uniform_location("u_matrix");

        let vertex_buffer = VertexBuffer::new(
            &gfx,
            &[
                VertexAttr::new(0, VertexFormat::Float3),
                VertexAttr::new(1, VertexFormat::Float4),
            ],
            DrawUsage::Dynamic,
        )?;

        let index_buffer = IndexBuffer::new(gfx, DrawUsage::Dynamic)?;

        let max_vertices = max_vertices(gfx);
        let batch_size = batch_vertices(vertex_buffer.offset());

        let vertices = vec![0.0; batch_size];
        let indices = vec![0; batch_size / vertex_buffer.offset()];

        Ok(Self {
            pipeline,
            vbo: vertex_buffer,
            ibo: index_buffer,
            matrix_loc,
            vertices,
            indices,
            index: 0,
            max_vertices,
            batch_size,
        })
    }

    fn check_batch_size(&mut self, gfx: &mut Graphics, data: &DrawData) {
        let next_size = self.vertices.len() + self.batch_size;
        let can_be_bigger = next_size < self.max_vertices;
        if can_be_bigger {
            let is_bigger = data.indices.len() > self.indices.len();
            let is_more = self.index + data.indices.len() >= self.indices.len();
            if is_bigger || is_more {
                self.flush(gfx, data.projection);

                let index_next_size = next_size / self.vbo.offset();
                log::debug!(
                    "ColorBatcher -> Increasing vertex_buffer to {} and index_buffer to {}",
                    next_size,
                    index_next_size
                );

                self.vertices.resize(next_size, 0.0);
                self.indices.resize(index_next_size, 0);
            }
        }
    }

    pub fn push_data(&mut self, gfx: &mut Graphics, data: DrawData) {
        self.check_batch_size(gfx, &data);

        // Check if the batch is bigger than the max_vertices allowed and split it
        if data.indices.len() > self.indices.len() {
            return self.split_batch(gfx, data);
        }

        // Flush if we reach the end of this batch
        let next_index = self.index + data.indices.len();
        if next_index >= self.indices.len() {
            self.flush(gfx, data.projection);
        }

        // Flush if we change the blend mode
        if self.pipeline.options.color_blend != data.blend {
            self.flush(gfx, data.projection);
            self.pipeline.options.color_blend = data.blend;
        }

        // Push the vertices on the current batch
        self.push_vertices(
            data.indices,
            data.vertices,
            &data.color,
            data.matrix,
            data.alpha,
        );
    }

    fn split_batch(&mut self, gfx: &mut Graphics, data: DrawData) {
        // TODO this doesn't care about indices...

        let mut indices = vec![0; self.indices.len()];
        let iterations = (data.indices.len() / self.indices.len()) + 1;

        for i in 0..iterations {
            let start = i * self.indices.len();
            let end = (start + self.indices.len()).min(data.indices.len());
            for (i, v) in (start..end).enumerate() {
                indices[i] = (v - start) as u32;
            }

            self.push_vertices(
                &indices[0..end - start],
                &data.vertices[start * 3..end * 3],
                &data.color,
                data.matrix,
                data.alpha,
            );

            self.flush(gfx, data.projection);
        }
    }

    fn push_vertices(
        &mut self,
        indices: &[u32],
        vertices: &[f32],
        color: &Color,
        matrix: &Matrix4,
        alpha: f32,
    ) {
        for (i, index) in indices.iter().enumerate() {
            self.indices[self.index + i] = self.index as u32 + *index;
        }

        let offset = self.vbo.offset();
        let [r, g, b, a] = color.to_rgba();
        let mut index_offset = self.index * offset;

        for (i, _) in vertices.iter().enumerate().step_by(3) {
            let [x, y, z, _] = matrix4_mul_vector4(
                matrix,
                &[vertices[i + 0], vertices[i + 1], vertices[i + 2], 1.0],
            );

            self.vertices[index_offset + 0] = x;
            self.vertices[index_offset + 1] = y;
            self.vertices[index_offset + 2] = z;
            self.vertices[index_offset + 3] = r;
            self.vertices[index_offset + 4] = g;
            self.vertices[index_offset + 5] = b;
            self.vertices[index_offset + 6] = a * alpha;

            index_offset += offset;
        }

        self.index += indices.len();
    }

    pub fn flush(&mut self, gfx: &mut Graphics, projection: &Matrix4) {
        if self.index == 0 {
            return;
        }

        gfx.set_pipeline(&self.pipeline);
        gfx.bind_vertex_buffer(&self.vbo, &self.vertices);
        gfx.bind_index_buffer(&self.ibo, &self.indices);
        gfx.bind_uniform(&self.matrix_loc, projection);
        gfx.draw(0, self.index as i32);
        self.index = 0;
    }
}