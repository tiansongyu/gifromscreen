using System.Windows;
using System.Windows.Controls;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace GifFromScreen.CinemagraphProbe;

internal sealed record BoundsInfo(bool Empty, double X, double Y, double Width, double Height);
internal sealed record ClipResult(RenderTargetBitmap Bitmap, BoundsInfo DescendantBounds, BoundsInfo UsedBounds);
internal sealed record PngResult(BitmapSource Working, byte[] Png, byte[] Rgba, double DecodedDpiX, double DecodedDpiY);

internal static class ProbeRenderer
{
    internal static BitmapSource Create(int width, int height, byte[] rgba)
    {
        Limits.Size(width, height);
        if (rgba.Length != checked(width * height * 4)) throw new InvalidDataException("Invalid source bytes.");
        var bgra = rgba.ToArray();
        for (var i = 0; i < bgra.Length; i += 4) (bgra[i], bgra[i + 2]) = (bgra[i + 2], bgra[i]);
        var bitmap = BitmapSource.Create(width, height, 96, 96, PixelFormats.Bgra32, null, bgra, width * 4);
        bitmap.Freeze();
        return bitmap;
    }

    // Main result follows the actual UIElement GetScaledRender overload at
    // ImageMethods.cs 2104-2163, not a supposedly equivalent PushClip shortcut.
    internal static ClipResult ClipImage(BitmapSource first, Geometry clip)
    {
        Require96(first);
        var image = new Image { Source = first, Clip = clip };
        image.Measure(new Size(first.Width, first.Height));
        image.Arrange(new Rect(image.DesiredSize));
        if (PresentationSource.FromVisual(image) is not null)
            throw new InvalidOperationException("The probe image must never attach to a presentation source.");
        var bounds = VisualTreeHelper.GetDescendantBounds(image);
        var original = Describe(bounds);
        if (bounds.IsEmpty) bounds = new Rect(new Point(0, 0), new Point(image.ActualWidth, image.ActualHeight));
        // source.Scale() is exactly 1 for this unattached visual, as upstream Other.cs 168.
        if (bounds.Width > first.PixelWidth) bounds.Width = first.PixelWidth;
        if (bounds.Height > first.PixelHeight) bounds.Height = first.PixelHeight;
        if (bounds.X < 0) bounds.X = 0;
        if (bounds.Y < 0) bounds.Y = 0;
        var visual = new DrawingVisual();
        using (var draw = visual.RenderOpen())
        {
            var brush = new VisualBrush(image) { AutoLayoutContent = false, Stretch = Stretch.Fill };
            draw.DrawRectangle(brush, null, new Rect(new Point(bounds.X, bounds.Y), new Size(bounds.Width, bounds.Height)));
        }
        return new ClipResult(Render(visual, first.PixelWidth, first.PixelHeight), original, Describe(bounds));
    }

    internal static RenderTargetBitmap PushClipDiagnostic(BitmapSource first, Geometry clip)
    {
        var visual = new DrawingVisual();
        using (var draw = visual.RenderOpen())
        {
            draw.PushClip(clip);
            draw.DrawImage(first, new Rect(0, 0, first.Width, first.Height));
            draw.Pop();
        }
        return Render(visual, first.PixelWidth, first.PixelHeight);
    }

    // OverlayAsync 5591-5628 receives the clipped RTB directly. Do not encode
    // or decode that clip before passing it into this primary path.
    internal static RenderTargetBitmap Overlay(BitmapSource current, BitmapSource clipped)
    {
        Require96(current);
        Require96(clipped);
        var visual = new DrawingVisual();
        using (var draw = visual.RenderOpen())
        {
            draw.DrawImage(current, new Rect(0, 0, current.Width, current.Height));
            draw.DrawImage(clipped, new Rect(0, 0, clipped.Width, clipped.Height));
        }
        return Render(visual, current.PixelWidth, current.PixelHeight);
    }

    internal static byte[] Pixels(BitmapSource bitmap, PixelFormat format)
    {
        Limits.Size(bitmap.PixelWidth, bitmap.PixelHeight);
        BitmapSource converted = bitmap.Format == format ? bitmap : new FormatConvertedBitmap(bitmap, format, null, 0);
        var stride = checked(bitmap.PixelWidth * 4);
        var pixels = new byte[checked(stride * bitmap.PixelHeight)];
        converted.CopyPixels(pixels, stride, 0);
        return pixels;
    }

    internal static PngResult PngRoundTrip(BitmapSource source)
    {
        var encoder = new PngBitmapEncoder();
        encoder.Frames.Add(BitmapFrame.Create(source));
        using var encoded = new MemoryStream();
        encoder.Save(encoded);
        if (encoded.Length > Limits.MaxOutputBytes) throw new InvalidDataException("PNG exceeds the artifact limit.");
        var png = encoded.ToArray();
        using var input = new MemoryStream(png, writable: false);
        var decoder = BitmapDecoder.Create(input, BitmapCreateOptions.PreservePixelFormat, BitmapCacheOption.OnLoad);
        if (decoder.Frames.Count != 1) throw new InvalidDataException("Expected one PNG frame.");
        var decoded = decoder.Frames[0];
        if (decoded.PixelWidth != source.PixelWidth || decoded.PixelHeight != source.PixelHeight)
            throw new InvalidDataException("PNG round trip changed dimensions.");
        var rgba = Pixels(decoded, PixelFormats.Bgra32);
        for (var i = 0; i < rgba.Length; i += 4) (rgba[i], rgba[i + 2]) = (rgba[i + 2], rgba[i]);
        return new PngResult(NormalizeDpi(decoded), png, rgba, decoded.DpiX, decoded.DpiY);
    }

    private static RenderTargetBitmap Render(Visual visual, int width, int height)
    {
        Limits.Check("before WPF RenderTargetBitmap");
        Limits.Size(width, height);
        var bitmap = new RenderTargetBitmap(width, height, 96, 96, PixelFormats.Pbgra32);
        bitmap.Render(visual);
        return (RenderTargetBitmap)bitmap.GetAsFrozen();
    }

    private static BoundsInfo Describe(Rect bounds) => bounds.IsEmpty
        ? new BoundsInfo(true, 0, 0, 0, 0) : new BoundsInfo(false, bounds.X, bounds.Y, bounds.Width, bounds.Height);

    private static void Require96(BitmapSource bitmap)
    {
        if (bitmap.DpiX != 96 || bitmap.DpiY != 96) throw new InvalidDataException("Working pixels must use exactly 96 DPI.");
    }

    // Same physical-space rule as the established suite, copied independently:
    // preserve decoded native-format pixels/palette and change only DPI metadata.
    private static BitmapSource NormalizeDpi(BitmapSource decoded)
    {
        double[] accepted = { 96, 3779 * .0254, 3780 * .0254, (double)(float)(3779 * .0254), (double)(float)(3780 * .0254) };
        if (!accepted.Any(v => Math.Abs(decoded.DpiX - v) <= 1e-9) || !accepted.Any(v => Math.Abs(decoded.DpiY - v) <= 1e-9))
            throw new InvalidDataException("Unexpected PNG density; refusing to conflate DPI and pixel differences.");
        var bits = decoded.Format.BitsPerPixel;
        if (bits is < 1 or > 128) throw new InvalidDataException("Unsupported PNG working format.");
        var stride = checked((decoded.PixelWidth * bits + 7) / 8);
        var pixels = new byte[checked(stride * decoded.PixelHeight)];
        decoded.CopyPixels(pixels, stride, 0);
        var working = BitmapSource.Create(decoded.PixelWidth, decoded.PixelHeight, 96, 96,
            decoded.Format, decoded.Palette, pixels, stride);
        working.Freeze();
        var verified = new byte[pixels.Length];
        working.CopyPixels(verified, stride, 0);
        var originalPalette = decoded.Palette?.Colors;
        var workingPalette = working.Palette?.Colors;
        var samePalette = originalPalette is null ? workingPalette is null
            : workingPalette is not null && originalPalette.SequenceEqual(workingPalette);
        if (!pixels.AsSpan().SequenceEqual(verified) || working.Format != decoded.Format || !samePalette)
            throw new InvalidDataException("DPI normalization changed native pixels, format or palette.");
        Require96(working);
        return working;
    }
}
