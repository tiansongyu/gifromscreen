using System.Globalization;
using System.Text.Json;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;
using System.Windows.Shapes;

namespace GifFromScreen.WpfReference;

// Real WPF Shapes; the two custom DefiningGeometry implementations are compiled
// unchanged from the separately checked-out, hash-pinned ScreenToGif source.
internal static class VectorShapesRenderer
{
    internal static BitmapSource Apply(BitmapSource input, VectorShapesOperation operation)
    {
        DpiNormalization.RequireWorkingDpi(input);
        var size = Limits.Size(input.PixelWidth, input.PixelHeight);
        var canvas = new Canvas
        {
            Width = size.Width, Height = size.Height,
            Background = Brushes.Transparent, ClipToBounds = true,
            // Generic.xaml sets pixel snapping on DrawingCanvas; Editor.xaml
            // sets inherited layout rounding on its Window. Keep both explicit.
            SnapsToDevicePixels = true, UseLayoutRounding = true,
        };
        foreach (var definition in operation.Shapes)
        {
            Limits.Check("vector shape construction");
            var shape = CreateShape(definition);
            Canvas.SetLeft(shape, definition.Bounds.XHundredths / 100.0);
            Canvas.SetTop(shape, definition.Bounds.YHundredths / 100.0);
            canvas.Children.Add(shape);
        }
        canvas.Measure(new Size(size.Width, size.Height));
        canvas.Arrange(new Rect(0, 0, size.Width, size.Height));
        canvas.UpdateLayout();
        foreach (Shape shape in canvas.Children)
        {
            var origin = shape.TransformToAncestor(canvas).Transform(new Point(0, 0));
            var offset = VisualTreeHelper.GetOffset(shape);
            // Bounded diagnostics from actual arranged WPF objects, not Rust.
            // The supervisor retains this log alongside the pixel artifacts.
            Console.WriteLine("VECTOR_LAYOUT " + JsonSerializer.Serialize(new
            {
                kind = shape.GetType().Name,
                requested = new[] { Canvas.GetLeft(shape), Canvas.GetTop(shape), shape.Width, shape.Height },
                desired = new[] { shape.DesiredSize.Width, shape.DesiredSize.Height },
                rendered = new[] { shape.RenderSize.Width, shape.RenderSize.Height },
                offset = new[] { offset.X, offset.Y },
                transformed_origin = new[] { origin.X, origin.Y },
                geometry = PathGeometry.CreateFromGeometry(shape.RenderedGeometry).ToString(CultureInfo.InvariantCulture),
                layout_clip = VisualTreeHelper.GetClip(shape) is Geometry clip
                    ? PathGeometry.CreateFromGeometry(clip).ToString(CultureInfo.InvariantCulture) : null,
                shape.SnapsToDevicePixels, shape.UseLayoutRounding,
            }));
        }

        // ImageMethods.GetScaledRender, in the explicitly measured scale=1,
        // dpi=96 space. Preserve its VisualBrush and bounds-clamping path;
        // do not replace the PM intermediate with a PNG/WIC round trip.
        var bounds = VisualTreeHelper.GetDescendantBounds(canvas);
        Console.WriteLine("VECTOR_CANVAS_BOUNDS " + bounds.ToString(CultureInfo.InvariantCulture));
        if (bounds.IsEmpty) bounds = new Rect(0, 0, canvas.ActualWidth, canvas.ActualHeight);
        bounds.Width = Math.Min(bounds.Width, size.Width);
        bounds.Height = Math.Min(bounds.Height, size.Height);
        bounds.X = Math.Max(bounds.X, 0);
        bounds.Y = Math.Max(bounds.Y, 0);
        var scaledVisual = new DrawingVisual();
        using (var draw = scaledVisual.RenderOpen())
        {
            var brush = new VisualBrush(canvas) { AutoLayoutContent = false, Stretch = Stretch.Fill };
            draw.DrawRectangle(brush, null, bounds);
        }
        var shapes = Render(scaledVisual, size);
        var composite = new DrawingVisual();
        using (var draw = composite.RenderOpen())
        {
            draw.DrawImage(input, new Rect(0, 0, input.Width, input.Height));
            draw.DrawImage(shapes, new Rect(0, 0, shapes.Width, shapes.Height));
        }
        return Render(composite, size);
    }

    private static Shape CreateShape(VectorShapeSpec definition)
    {
        Shape shape = definition.Kind switch
        {
            VectorKind.Rectangle => new Rectangle
            {
                RadiusX = definition.CornerRadiusHundredths / 100.0,
                RadiusY = definition.CornerRadiusHundredths / 100.0,
            },
            VectorKind.Ellipse => new Ellipse(),
            VectorKind.Triangle => new ScreenToGif.Controls.Shapes.Triangle(),
            VectorKind.BlockArrow => new ScreenToGif.Controls.Shapes.Arrow(),
            _ => throw new InvalidDataException("Unsupported vector shape."),
        };
        shape.Width = definition.Bounds.WidthHundredths / 100.0;
        shape.Height = definition.Bounds.HeightHundredths / 100.0;
        shape.StrokeThickness = definition.StrokeWidthHundredths / 100.0;
        shape.Stroke = new SolidColorBrush(definition.Stroke.ToColor());
        shape.Fill = definition.Fill is Rgba fill ? new SolidColorBrush(fill.ToColor()) : null;
        // ElementAdorner uses relative RenderTransformOrigin, so rotation follows
        // the actual arranged RenderSize, not the possibly fractional Width/Height.
        shape.RenderTransformOrigin = new Point(0.5, 0.5);
        shape.RenderTransform = new RotateTransform(definition.RotationHundredths / 100.0);
        return shape;
    }

    private static RenderTargetBitmap Render(Visual visual, PixelSize size)
    {
        Limits.Check("vector RenderTargetBitmap");
        var bitmap = new RenderTargetBitmap(size.Width, size.Height, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(visual);
        bitmap.Freeze();
        Limits.Check("vector RenderTargetBitmap complete");
        return bitmap;
    }
}
