using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Effects;
using System.Windows.Media.Imaging;

namespace GifFromScreen.WpfReference;

// Public WPF operations only. Geometry independently expresses the 96-DPI
// ScreenToGif a4d0a67 BorderAsync/ShadowAsync contract; no Rust kernel or pixel
// arithmetic is used to manufacture the reference image.
internal static class WpfRenderer
{
    internal static BitmapSource CreateBitmap(ImageSpec image)
    {
        Limits.Size(image.Width, image.Height);
        var bgra = image.Rgba.ToArray();
        for (var index = 0; index < bgra.Length; index += 4)
            (bgra[index], bgra[index + 2]) = (bgra[index + 2], bgra[index]);
        var bitmap = BitmapSource.Create(image.Width, image.Height, 96, 96, PixelFormats.Bgra32,
            null, bgra, checked(image.Width * 4));
        bitmap.Freeze();
        return bitmap;
    }

    internal static PixelSize OutputSize(PixelSize input, Operation operation) => operation switch
    {
        BorderOperation border => BorderLayout.Create(input, border.Style).Output,
        ShadowOperation shadow => ShadowLayout.Create(input, shadow.Style).Output,
        OverlayOperation => Limits.Size(input.Width, input.Height),
        _ => throw new InvalidDataException("Unsupported operation."),
    };

    internal static BitmapSource Apply(BitmapSource input, Operation operation)
    {
        DpiNormalization.RequireWorkingDpi(input);
        return operation switch
        {
            BorderOperation border => ApplyBorder(input, border.Style),
            ShadowOperation shadow => ApplyShadow(input, shadow.Style),
            OverlayOperation overlay => ApplyOverlay(input, overlay),
            _ => throw new InvalidDataException("Unsupported operation."),
        };
    }

    private static BitmapSource ApplyBorder(BitmapSource input, BorderStyle style)
    {
        var layout = BorderLayout.Create(new PixelSize(input.PixelWidth, input.PixelHeight), style);
        var visual = new DrawingVisual();
        using (var draw = visual.RenderOpen())
        {
            // Deliberately not the rounded RTB size: only left/top are cast
            // before adding the opposite exterior edge in the pinned Apply.
            draw.DrawRectangle(Brushes.White, null, new Rect(0, 0, layout.BackgroundWidth, layout.BackgroundHeight));
            draw.DrawImage(input, new Rect(layout.SourceX, layout.SourceY, input.Width, input.Height));
            var brush = new SolidColorBrush(style.Color.ToColor());
            Line(draw, brush, layout.Left, new Point(Math.Abs(layout.Left) / 2, 0),
                new Point(Math.Abs(layout.Left) / 2, layout.BackgroundHeight));
            var rightX = Math.Max(-layout.Left, 0) + input.Width - layout.Right / 2;
            Line(draw, brush, layout.Right, new Point(rightX, 0), new Point(rightX, layout.BackgroundHeight));
            var horizontalStart = Math.Abs(layout.Left);
            var horizontalEnd = input.Width + layout.SourceX - Math.Max(layout.Right, 0);
            Line(draw, brush, layout.Top, new Point(horizontalStart, Math.Abs(layout.Top) / 2),
                new Point(horizontalEnd, Math.Abs(layout.Top) / 2));
            var bottomY = Math.Max(-layout.Top, 0) + input.Height - layout.Bottom / 2;
            Line(draw, brush, layout.Bottom, new Point(horizontalStart, bottomY), new Point(horizontalEnd, bottomY));
        }
        return Render(visual, layout.Output);
    }

    private static void Line(DrawingContext draw, Brush brush, double thickness, Point start, Point end) =>
        draw.DrawLine(new Pen(brush, Math.Abs(thickness)), start, end);

    private static BitmapSource ApplyShadow(BitmapSource input, ShadowStyle style)
    {
        var layout = ShadowLayout.Create(new PixelSize(input.PixelWidth, input.PixelHeight), style);
        var visual = new DrawingVisual
        {
            Effect = new DropShadowEffect
            {
                Color = style.Color.ToColor(),
                BlurRadius = style.BlurRadiusHundredths / 100.0,
                ShadowDepth = style.DepthHundredths / 100.0,
                Direction = style.DirectionHundredths / 100.0,
                Opacity = style.OpacityBasisPoints / 10_000.0,
                RenderingBias = RenderingBias.Quality,
            },
        };
        using (var draw = visual.RenderOpen())
            draw.DrawImage(input, new Rect(layout.SourceX, layout.SourceY, input.Width, input.Height));
        var withShadow = Render(visual, layout.Output);
        Limits.Check("shadow intermediate");
        var background = new DrawingVisual();
        using (var draw = background.RenderOpen())
        {
            draw.DrawRectangle(new SolidColorBrush(style.Background.ToColor()), null,
                new Rect(0, 0, withShadow.Width, withShadow.Height));
            draw.DrawImage(withShadow, new Rect(0, 0, withShadow.Width, withShadow.Height));
        }
        return Render(background, layout.Output);
    }

    private static BitmapSource ApplyOverlay(BitmapSource input, OverlayOperation overlay)
    {
        var image = CreateBitmap(overlay.Image);
        var visual = new DrawingVisual();
        using (var draw = visual.RenderOpen())
        {
            draw.DrawImage(input, new Rect(0, 0, input.Width, input.Height));
            draw.DrawImage(image, new Rect(overlay.X, overlay.Y, image.Width, image.Height));
        }
        return Render(visual, new PixelSize(input.PixelWidth, input.PixelHeight));
    }

    private static RenderTargetBitmap Render(Visual visual, PixelSize size)
    {
        Limits.Size(size.Width, size.Height);
        Limits.Check("before RenderTargetBitmap");
        var output = new RenderTargetBitmap(size.Width, size.Height, 96, 96, PixelFormats.Pbgra32);
        output.Render(visual);
        output.Freeze();
        Limits.Check("after RenderTargetBitmap");
        return output;
    }

    private sealed record BorderLayout(double Left, double Top, double Right, double Bottom,
        int SourceX, int SourceY, double BackgroundWidth, double BackgroundHeight, PixelSize Output)
    {
        internal static BorderLayout Create(PixelSize input, BorderStyle style)
        {
            var left = style.Widths.LeftMilli / 1000.0;
            var top = style.Widths.TopMilli / 1000.0;
            var right = style.Widths.RightMilli / 1000.0;
            var bottom = style.Widths.BottomMilli / 1000.0;
            var sourceX = (int)Math.Abs(Math.Min(left, 0));
            var sourceY = (int)Math.Abs(Math.Min(top, 0));
            var width = input.Width + Math.Round(Math.Max(-left, 0) + Math.Max(-right, 0), MidpointRounding.ToEven);
            var height = input.Height + Math.Round(Math.Max(-top, 0) + Math.Max(-bottom, 0), MidpointRounding.ToEven);
            var output = CheckedSize(width, height);
            var backgroundWidth = input.Width + Math.Abs((int)Math.Min(left, 0) + Math.Min(right, 0));
            var backgroundHeight = input.Height + Math.Abs((int)Math.Min(top, 0) + Math.Min(bottom, 0));
            return new BorderLayout(left, top, right, bottom, sourceX, sourceY, backgroundWidth, backgroundHeight, output);
        }
    }

    private sealed record ShadowLayout(int SourceX, int SourceY, PixelSize Output)
    {
        internal static ShadowLayout Create(PixelSize input, ShadowStyle style)
        {
            var radians = Math.PI / 180 * (style.DirectionHundredths / 100.0);
            var depth = style.DepthHundredths / 100.0;
            var x = depth * Math.Cos(radians);
            var yUp = depth * Math.Sin(radians);
            var halfBlur = style.BlurRadiusHundredths / 200.0;
            var left = halfBlur + Math.Max(-x, 0);
            var right = halfBlur + Math.Max(x, 0);
            var top = halfBlur + Math.Max(yUp, 0);
            var bottom = halfBlur + Math.Max(-yUp, 0);
            var output = CheckedSize(Math.Floor(left + input.Width + right), Math.Floor(top + input.Height + bottom));
            return new ShadowLayout((int)left, (int)top, output);
        }
    }

    private static PixelSize CheckedSize(double width, double height)
    {
        if (!double.IsFinite(width) || !double.IsFinite(height) || width < 1 || height < 1
            || width > Limits.MaxDimension || height > Limits.MaxDimension)
            throw new InvalidDataException("Predicted WPF bitmap exceeds the 256x256 reference limit.");
        return Limits.Size(checked((int)width), checked((int)height));
    }
}
