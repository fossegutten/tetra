pub use lyon_tessellation::path::builder::BorderRadii;

use std::rc::Rc;

use bytemuck::{Pod, Zeroable};
use lyon_tessellation::geom::euclid::{Point2D, Size2D};
use lyon_tessellation::math::{Angle, Point, Rect, Vector};
use lyon_tessellation::path::builder::{Build, PathBuilder};
use lyon_tessellation::path::{Polygon, Winding};
use lyon_tessellation::{
    BuffersBuilder, FillOptions, FillTessellator, FillVertex, FillVertexConstructor, StrokeOptions,
    StrokeTessellator, StrokeVertex, StrokeVertexConstructor, VertexBuffers,
};

use crate::graphics::{self, ActiveShader, Color, DrawParams, Drawable, Rectangle, Texture};
use crate::math::{Mat4, Vec2, Vec3};
use crate::platform::{RawIndexBuffer, RawVertexBuffer};
use crate::Context;
use crate::{Result, TetraError};

/// An individual piece of vertex data.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct Vertex {
    /// The position of the vertex, in screen co-ordinates.
    ///
    /// The transform matrix will be applied to this value, followed by a projection
    /// from screen co-ordinates to device co-ordinates.
    pub position: Vec2<f32>,

    /// The texture co-ordinates that should be sampled for this vertex.
    ///
    /// Both the X and the Y should be between 0.0 and 1.0.
    pub uv: Vec2<f32>,

    /// The color of the vertex.
    ///
    /// This will be multiplied by the `color` of the `DrawParams` when drawing a
    /// mesh.
    pub color: Color,
}

impl Vertex {
    /// Creates a new vertex.
    pub fn new(position: Vec2<f32>, uv: Vec2<f32>, color: Color) -> Vertex {
        Vertex {
            position,
            uv,
            color,
        }
    }
}

// SAFETY: While the contract for `Pod` states that all fields should also be `Pod`,
// that isn't possible without upstream changes. All of the fields meet the
// *requirements* to be `Pod`, however, so this should not be unsound.
unsafe impl Pod for Vertex {}
unsafe impl Zeroable for Vertex {}

/// The expected usage of a GPU buffer.
///
/// The GPU may optionally use this to optimize data storage and access.
pub enum BufferUsage {
    /// The buffer's data is not expected to change after creation.
    Static,

    /// The buffer's data is expected to change occasionally after creation.
    Dynamic,

    /// The buffer's data is expected to change every frame.
    Stream,
}

/// The ordering of the vertices in a piece of geometry.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum VertexWinding {
    /// The vertices are in clockwise order.
    Clockwise,

    /// The vertices are in counter-clockwise order.
    CounterClockwise,
}

impl VertexWinding {
    /// Returns the opposite winding, compared to `self`.
    pub fn flipped(self) -> VertexWinding {
        match self {
            VertexWinding::Clockwise => VertexWinding::CounterClockwise,
            VertexWinding::CounterClockwise => VertexWinding::Clockwise,
        }
    }
}

/// Vertex data, stored in GPU memory.
///
/// This data can be drawn to the screen via a [`Mesh`].
///
/// # Performance
///
/// Creating a `VertexBuffer` is a relatively expensive operation. If you can, store them in your
/// [`State`](crate::State) struct rather than recreating them each frame.
///
/// Cloning a `VertexBuffer` is a very cheap operation, as the underlying data is shared between the
/// original instance and the clone via [reference-counting](https://doc.rust-lang.org/std/rc/struct.Rc.html).
/// This does mean, however, that updating a `VertexBuffer` will also update any other clones of
/// that `VertexBuffer`.
///
#[derive(Clone, Debug, PartialEq)]
pub struct VertexBuffer {
    handle: Rc<RawVertexBuffer>,
}

impl VertexBuffer {
    /// Creates a new vertex buffer.
    ///
    /// The buffer will be created with the [`BufferUsage::Dynamic`] usage hint - this can
    /// be overridden via the [`with_usage`](Self::with_usage) constructor.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn new(ctx: &mut Context, vertices: &[Vertex]) -> Result<VertexBuffer> {
        VertexBuffer::with_usage(ctx, vertices, BufferUsage::Dynamic)
    }

    /// Creates a new vertex buffer, with the specified usage hint.
    ///
    /// The GPU may optionally use the usage hint to optimize data storage and access.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn with_usage(
        ctx: &mut Context,
        vertices: &[Vertex],
        usage: BufferUsage,
    ) -> Result<VertexBuffer> {
        let buffer = ctx.device.new_vertex_buffer(vertices.len(), 8, usage)?;

        ctx.device
            .set_vertex_buffer_data(&buffer, bytemuck::cast_slice(vertices), 0);

        Ok(VertexBuffer {
            handle: Rc::new(buffer),
        })
    }

    /// Uploads new vertex data to the GPU.
    ///
    /// # Panics
    ///
    /// Panics if the offset is out of bounds.
    pub fn set_data(&self, ctx: &mut Context, vertices: &[Vertex], offset: usize) {
        ctx.device
            .set_vertex_buffer_data(&self.handle, bytemuck::cast_slice(vertices), offset);
    }

    /// Creates a mesh using this buffer.
    ///
    /// This is a shortcut for calling [`Mesh::new`].
    pub fn into_mesh(self) -> Mesh {
        Mesh::new(self)
    }
}

/// Index data, stored in GPU memory.
///
/// An index buffer can be used as part of a [`Mesh`], in order to describe which vertex data should be drawn,
/// and what order it should be drawn in.
///
/// For example, to draw a square with raw vertex data, you need to use six vertices (two triangles,
/// with three vertices each). This is inefficient, as two of those vertices are shared by the two
/// triangles! Using an index buffer, you can instruct the graphics card to use vertices
/// multiple times while constructing your square.
///
/// Index data is made up of [`u32`] values, each of which correspond to the zero-based index of a vertex.
/// For example, to get the mesh to draw the third vertex, then the first, then the second, you would
/// create an index buffer containing `[2, 0, 1]`.
///
/// # Performance
///
/// Creating an `IndexBuffer` is a relatively expensive operation. If you can, store them in your
/// [`State`](crate::State) struct rather than recreating them each frame.
///
/// Cloning an `IndexBuffer` is a very cheap operation, as the underlying data is shared between the
/// original instance and the clone via [reference-counting](https://doc.rust-lang.org/std/rc/struct.Rc.html).
/// This does mean, however, that updating an `IndexBuffer` will also update any other clones of
/// that `IndexBuffer`.
#[derive(Clone, Debug, PartialEq)]
pub struct IndexBuffer {
    handle: Rc<RawIndexBuffer>,
}

impl IndexBuffer {
    /// Creates a new index buffer.
    ///
    /// The buffer will be created with the [`BufferUsage::Dynamic`] usage hint - this can
    /// be overridden via the [`with_usage`](Self::with_usage) constructor.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn new(ctx: &mut Context, indices: &[u32]) -> Result<IndexBuffer> {
        IndexBuffer::with_usage(ctx, indices, BufferUsage::Dynamic)
    }

    /// Creates a new index buffer, with the specified usage hint.
    ///
    /// The GPU may optionally use the usage hint to optimize data storage and access.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn with_usage(
        ctx: &mut Context,
        indices: &[u32],
        usage: BufferUsage,
    ) -> Result<IndexBuffer> {
        let buffer = ctx.device.new_index_buffer(indices.len(), usage)?;

        ctx.device.set_index_buffer_data(&buffer, indices, 0);

        Ok(IndexBuffer {
            handle: Rc::new(buffer),
        })
    }

    /// Sends new index data to the GPU.
    ///
    /// # Panics
    ///
    /// Panics if the offset is out of bounds.
    pub fn set_data(&self, ctx: &mut Context, indices: &[u32], offset: usize) {
        ctx.device
            .set_index_buffer_data(&self.handle, indices, offset);
    }
}

#[derive(Copy, Clone, Debug)]
struct DrawRange {
    start: usize,
    count: usize,
}

/// Ways of drawing a shape.
#[derive(Copy, Clone, Debug)]
pub enum ShapeStyle {
    /// A filled shape.
    Fill,
    /// An outlined shape with the specified stroke width.
    Stroke(f32),
}

/// A 2D mesh that can be drawn to the screen.
///
/// A `Mesh` is a wrapper for a [`VertexBuffer`], which allows it to be drawn in combination with three
/// optional modifiers:
///
/// * A [`Texture`] that individual vertices can sample from.
/// * An [`IndexBuffer`] that can be used to modify the order/subset of vertices that are drawn.
/// * A draw range, which can be used to draw subsections of the mesh.
///
/// Without a texture set, the mesh will be drawn in white - the `color` attribute on the [vertex data](Vertex) or
/// [`DrawParams`] can be used to change this.
///
/// Note that, unlike quad rendering via [`Texture`], mesh rendering is not batched by default - each time you
/// draw the mesh will result in a seperate draw call.
///
/// # Performance
///
/// Creating or cloning a `Mesh` is a very cheap operation, as meshes are effectively just collections
/// of resources that live on the GPU. The only expensive part is the creation of the buffers/textures,
/// which can be done ahead of time.
///
/// Note that cloned meshes do not share data, so updating one instance of a mesh will not affect
/// other instances.
#[derive(Clone, Debug)]
pub struct Mesh {
    vertex_buffer: VertexBuffer,
    index_buffer: Option<IndexBuffer>,
    texture: Option<Texture>,
    draw_range: Option<DrawRange>,
}

impl Mesh {
    /// Creates a new mesh, using the provided vertex buffer.
    pub fn new(vertex_buffer: VertexBuffer) -> Mesh {
        Mesh {
            vertex_buffer,
            index_buffer: None,
            texture: None,
            draw_range: None,
        }
    }

    /// Creates a new mesh, using the provided vertex and index buffers.
    pub fn indexed(vertex_buffer: VertexBuffer, index_buffer: IndexBuffer) -> Mesh {
        Mesh {
            vertex_buffer,
            index_buffer: Some(index_buffer),
            texture: None,
            draw_range: None,
        }
    }

    /// Creates a new rectangle mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn rectangle(ctx: &mut Context, style: ShapeStyle, rectangle: Rectangle) -> Result<Mesh> {
        GeometryBuilder::new()
            .rectangle(style, rectangle)?
            .build_mesh(ctx)
    }

    /// Creates a new rounded rectangle mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn rounded_rectangle(
        ctx: &mut Context,
        style: ShapeStyle,
        rectangle: Rectangle,
        radii: BorderRadii,
    ) -> Result<Mesh> {
        GeometryBuilder::new()
            .rounded_rectangle(style, rectangle, radii)?
            .build_mesh(ctx)
    }

    /// Creates a new circle mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn circle(
        ctx: &mut Context,
        style: ShapeStyle,
        center: Vec2<f32>,
        radius: f32,
    ) -> Result<Mesh> {
        GeometryBuilder::new()
            .circle(style, center, radius)?
            .build_mesh(ctx)
    }

    /// Creates a new ellipse mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn ellipse(
        ctx: &mut Context,
        style: ShapeStyle,
        center: Vec2<f32>,
        radii: Vec2<f32>,
    ) -> Result<Mesh> {
        GeometryBuilder::new()
            .ellipse(style, center, radii)?
            .build_mesh(ctx)
    }

    /// Creates a new polygon mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn polygon(ctx: &mut Context, style: ShapeStyle, points: &[Vec2<f32>]) -> Result<Mesh> {
        GeometryBuilder::new()
            .polygon(style, points)?
            .build_mesh(ctx)
    }

    /// Creates a new polyline mesh.
    ///
    /// If you need to draw multiple shapes, consider using [`GeometryBuilder`] to generate a combined mesh
    /// instead.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn polyline(ctx: &mut Context, stroke_width: f32, points: &[Vec2<f32>]) -> Result<Mesh> {
        GeometryBuilder::new()
            .polyline(stroke_width, points)?
            .build_mesh(ctx)
    }

    /// Gets a reference to the vertex buffer contained within this mesh.
    pub fn vertex_buffer(&self) -> &VertexBuffer {
        &self.vertex_buffer
    }

    /// Sets the vertex buffer that will be used when drawing the mesh.
    pub fn set_vertex_buffer(&mut self, vertex_buffer: VertexBuffer) {
        self.vertex_buffer = vertex_buffer;
    }

    /// Gets a reference to the index buffer contained within this mesh.
    ///
    /// Returns [`None`] if this mesh does not currently have an index buffer attatched.
    pub fn index_buffer(&self) -> Option<&IndexBuffer> {
        self.index_buffer.as_ref()
    }

    /// Sets the index buffer that will be used when drawing the mesh.
    pub fn set_index_buffer(&mut self, index_buffer: IndexBuffer) {
        self.index_buffer = Some(index_buffer);
    }

    /// Resets the mesh to no longer use indexed drawing.
    pub fn reset_index_buffer(&mut self) {
        self.index_buffer = None;
    }

    /// Gets a reference to the texture contained within this mesh.
    ///
    /// Returns [`None`] if this mesh does not currently have an texture attatched.
    pub fn texture(&self) -> Option<&Texture> {
        self.texture.as_ref()
    }

    /// Sets the texture that will be used when drawing the mesh.
    pub fn set_texture(&mut self, texture: Texture) {
        self.texture = Some(texture);
    }

    /// Resets the mesh to be untextured.
    pub fn reset_texture(&mut self) {
        self.texture = None;
    }

    /// Sets the range of vertices (or indices, if the mesh is indexed) that should be included
    /// when drawing this mesh.
    ///
    /// This can be useful if you have a large mesh but you only want to want to draw a
    /// subsection of it, or if you want to draw a mesh in multiple stages.
    pub fn set_draw_range(&mut self, start: usize, count: usize) {
        self.draw_range = Some(DrawRange { start, count });
    }

    /// Sets the mesh to include all of its data when drawing.
    pub fn reset_draw_range(&mut self) {
        self.draw_range = None;
    }
}

impl From<VertexBuffer> for Mesh {
    fn from(buffer: VertexBuffer) -> Self {
        Mesh::new(buffer)
    }
}

impl Drawable for Mesh {
    fn draw<P>(&self, ctx: &mut Context, params: P)
    where
        P: Into<DrawParams>,
    {
        graphics::flush(ctx);

        let texture = match &self.texture {
            Some(t) => t,
            None => &ctx.graphics.default_texture,
        };

        let shader = match &ctx.graphics.shader {
            ActiveShader::Default => &ctx.graphics.default_shader,
            ActiveShader::User(s) => s,
        };

        let params = params.into();

        let mut transform: Mat4<f32> = Mat4::translation_2d(-params.origin);
        transform.scale_3d(Vec3::from(params.scale));
        transform.rotate_z(params.rotation);
        transform.translate_2d(params.position);

        // TODO: Failing to apply the defaults should be handled more gracefully than this,
        // but we can't do that without breaking changes.
        let _ = shader.set_default_uniforms(
            &mut ctx.device,
            ctx.graphics.projection_matrix * ctx.graphics.transform_matrix * transform,
            params.color,
        );

        let draw_range = self.draw_range.map(|r| (r.start, r.count));

        match &self.index_buffer {
            Some(index_buffer) => {
                let (start, count) = draw_range.unwrap_or_else(|| (0, index_buffer.handle.count()));

                ctx.device.draw_elements(
                    &self.vertex_buffer.handle,
                    &index_buffer.handle,
                    &texture.data.handle,
                    &shader.data.handle,
                    start,
                    count,
                );
            }
            None => {
                let (start, count) =
                    draw_range.unwrap_or_else(|| (0, self.vertex_buffer.handle.count()));

                ctx.device.draw_arrays(
                    &self.vertex_buffer.handle,
                    &texture.data.handle,
                    &shader.data.handle,
                    start,
                    count,
                );
            }
        }
    }
}

fn to_lyon_rect(rectangle: Rectangle) -> Rect {
    Rect::new(
        Point2D::new(rectangle.x, rectangle.y),
        Size2D::new(rectangle.width, rectangle.height),
    )
}

struct TetraVertexConstructor(Color);

impl FillVertexConstructor<Vertex> for TetraVertexConstructor {
    fn new_vertex(&mut self, vertex: FillVertex) -> Vertex {
        let position = vertex.position();

        Vertex::new(Vec2::new(position.x, position.y), Vec2::zero(), self.0)
    }
}

impl StrokeVertexConstructor<Vertex> for TetraVertexConstructor {
    fn new_vertex(&mut self, vertex: StrokeVertex) -> Vertex {
        let position = vertex.position();

        Vertex::new(Vec2::new(position.x, position.y), Vec2::zero(), self.0)
    }
}

/// A builder for creating primitive shape geometry, and associated buffers/meshes.
#[derive(Debug, Clone)]
pub struct GeometryBuilder {
    data: VertexBuffers<Vertex, u32>,
    color: Color,
}

impl GeometryBuilder {
    /// Creates a new empty geometry builder.
    pub fn new() -> GeometryBuilder {
        GeometryBuilder {
            data: VertexBuffers::new(),
            color: Color::WHITE,
        }
    }

    /// Adds a rectangle.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn rectangle(
        &mut self,
        style: ShapeStyle,
        rectangle: Rectangle,
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        match style {
            ShapeStyle::Fill => {
                let options = FillOptions::default();
                let mut tessellator = FillTessellator::new();
                tessellator
                    .tessellate_rectangle(&to_lyon_rect(rectangle), &options, &mut builder)
                    .map_err(TetraError::TessellationError)?;
            }

            ShapeStyle::Stroke(width) => {
                let options = StrokeOptions::default().with_line_width(width);
                let mut tessellator = StrokeTessellator::new();
                tessellator
                    .tessellate_rectangle(&to_lyon_rect(rectangle), &options, &mut builder)
                    .map_err(TetraError::TessellationError)?;
            }
        }

        Ok(self)
    }

    /// Adds a rounded rectangle.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn rounded_rectangle(
        &mut self,
        style: ShapeStyle,
        rectangle: Rectangle,
        radii: BorderRadii,
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        match style {
            ShapeStyle::Fill => {
                let options = FillOptions::default();
                let mut tessellator = FillTessellator::new();
                let mut builder = tessellator.builder(&options, &mut builder);
                builder.add_rounded_rectangle(&to_lyon_rect(rectangle), &radii, Winding::Positive);
                builder.build().map_err(TetraError::TessellationError)?;
            }

            ShapeStyle::Stroke(width) => {
                let options = StrokeOptions::default().with_line_width(width);
                let mut tessellator = StrokeTessellator::new();
                let mut builder = tessellator.builder(&options, &mut builder);
                builder.add_rounded_rectangle(&to_lyon_rect(rectangle), &radii, Winding::Positive);
                builder.build().map_err(TetraError::TessellationError)?;
            }
        }

        Ok(self)
    }

    /// Adds a circle.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn circle(
        &mut self,
        style: ShapeStyle,
        center: Vec2<f32>,
        radius: f32,
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        match style {
            ShapeStyle::Fill => {
                let options = FillOptions::default();
                let mut tessellator = FillTessellator::new();

                tessellator
                    .tessellate_circle(
                        Point::new(center.x, center.y),
                        radius,
                        &options,
                        &mut builder,
                    )
                    .map_err(TetraError::TessellationError)?;
            }

            ShapeStyle::Stroke(width) => {
                let options = StrokeOptions::default().with_line_width(width);
                let mut tessellator = StrokeTessellator::new();

                tessellator
                    .tessellate_circle(
                        Point::new(center.x, center.y),
                        radius,
                        &options,
                        &mut builder,
                    )
                    .map_err(TetraError::TessellationError)?;
            }
        }

        Ok(self)
    }

    /// Adds an ellipse.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn ellipse(
        &mut self,
        style: ShapeStyle,
        center: Vec2<f32>,
        radii: Vec2<f32>,
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        match style {
            ShapeStyle::Fill => {
                let options = FillOptions::default();
                let mut tessellator = FillTessellator::new();

                tessellator
                    .tessellate_ellipse(
                        Point::new(center.x, center.y),
                        Vector::new(radii.x, radii.y),
                        Angle::radians(0.0),
                        Winding::Positive,
                        &options,
                        &mut builder,
                    )
                    .map_err(TetraError::TessellationError)?;
            }

            ShapeStyle::Stroke(width) => {
                let options = StrokeOptions::default().with_line_width(width);
                let mut tessellator = StrokeTessellator::new();

                tessellator
                    .tessellate_ellipse(
                        Point::new(center.x, center.y),
                        Vector::new(radii.x, radii.y),
                        Angle::radians(0.0),
                        Winding::Positive,
                        &options,
                        &mut builder,
                    )
                    .map_err(TetraError::TessellationError)?;
            }
        }

        Ok(self)
    }

    /// Adds a polygon.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn polygon(
        &mut self,
        style: ShapeStyle,
        points: &[Vec2<f32>],
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        let points: Vec<Point> = points
            .iter()
            .map(|point| Point::new(point.x, point.y))
            .collect();

        let polygon = Polygon {
            points: &points,
            closed: true,
        };

        match style {
            ShapeStyle::Fill => {
                let options = FillOptions::default();
                let mut tessellator = FillTessellator::new();

                tessellator
                    .tessellate_polygon(polygon, &options, &mut builder)
                    .map_err(TetraError::TessellationError)?;
            }

            ShapeStyle::Stroke(width) => {
                let options = StrokeOptions::default().with_line_width(width);
                let mut tessellator = StrokeTessellator::new();

                tessellator
                    .tessellate_polygon(polygon, &options, &mut builder)
                    .map_err(TetraError::TessellationError)?;
            }
        }

        Ok(self)
    }

    /// Adds a polyline.
    ///
    /// # Errors
    ///
    /// * [`TetraError::TessellationError`](crate::TetraError::TessellationError) will be returned if the shape
    /// could not be turned into vertex data.
    pub fn polyline(
        &mut self,
        stroke_width: f32,
        points: &[Vec2<f32>],
    ) -> Result<&mut GeometryBuilder> {
        let mut builder = BuffersBuilder::new(&mut self.data, TetraVertexConstructor(self.color));

        let points: Vec<Point> = points
            .iter()
            .map(|point| Point::new(point.x, point.y))
            .collect();

        let polygon = Polygon {
            points: &points,
            closed: false,
        };

        let options = StrokeOptions::default().with_line_width(stroke_width);
        let mut tessellator = StrokeTessellator::new();

        tessellator
            .tessellate_polygon(polygon, &options, &mut builder)
            .map_err(TetraError::TessellationError)?;

        Ok(self)
    }

    /// Sets the color that will be used for subsequent shapes.
    ///
    /// You can also use [`DrawParams::color`](super::DrawParams) to tint an entire mesh -
    /// this method only needs to be used if you want to display multiple colors in a
    /// single piece of geometry.
    pub fn set_color(&mut self, color: Color) -> &mut GeometryBuilder {
        self.color = color;
        self
    }

    /// Clears the geometry builder's data.
    pub fn clear(&mut self) -> &mut GeometryBuilder {
        self.data.vertices.clear();
        self.data.indices.clear();

        self
    }

    /// Returns a view of the generated vertex data.
    pub fn vertices(&self) -> &[Vertex] {
        &self.data.vertices
    }

    /// Returns a view of the generated index data.
    pub fn indices(&self) -> &[u32] {
        &self.data.indices
    }

    /// Consumes the builder, returning the generated geometry.
    pub fn into_data(self) -> (Vec<Vertex>, Vec<u32>) {
        (self.data.vertices, self.data.indices)
    }

    /// Builds a vertex and index buffer from the generated geometry.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn build_buffers(&self, ctx: &mut Context) -> Result<(VertexBuffer, IndexBuffer)> {
        Ok((
            VertexBuffer::new(ctx, &self.data.vertices)?,
            IndexBuffer::new(ctx, &self.data.indices)?,
        ))
    }

    /// Builds a mesh from the generated geometry.
    ///
    /// # Errors
    ///
    /// * [`TetraError::PlatformError`](crate::TetraError::PlatformError) will be returned if the underlying
    /// graphics API encounters an error.
    pub fn build_mesh(&self, ctx: &mut Context) -> Result<Mesh> {
        let (vertex_buffer, index_buffer) = self.build_buffers(ctx)?;

        Ok(Mesh::indexed(vertex_buffer, index_buffer))
    }
}

impl Default for GeometryBuilder {
    fn default() -> Self {
        GeometryBuilder::new()
    }
}
