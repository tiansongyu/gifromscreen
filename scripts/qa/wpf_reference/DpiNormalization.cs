using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace GifFromScreen.WpfReference;

// PNG pHYs stores integer pixels/metre, so 96 DPI cannot be represented exactly.
// The observed WIC encoder writes 3779 ppm (95.9866 DPI); nearest rounding can
// instead yield 3780 ppm (96.012 DPI). Neither changes the decoded pixel array.
// This harness compares operations in a deliberately fixed physical-pixel space,
// not the upstream editor's separate, historical mixing of rounded UI DPI and DIP.
internal static class DpiNormalization
{
    private const double WorkingDpi = 96.0;
    private const double Epsilon = 0.000_000_001;
    // WPF may expose native single-precision density promoted to a double.
    // Run 34070959557 reported 95.98660278320312, exactly that representation.
    private static readonly double[] AcceptedDecodedDpi = {
        WorkingDpi, 3779 * 0.0254, 3780 * 0.0254,
        (double)(float)(3779 * 0.0254), (double)(float)(3780 * 0.0254),
    };

    internal static void RequireWorkingDpi(BitmapSource source)
    {
        if (source.DpiX != WorkingDpi || source.DpiY != WorkingDpi)
            throw new InvalidDataException(FormattableString.Invariant(
                $"Working bitmap must be exactly 96 DPI, not {source.DpiX:R}x{source.DpiY:R}."));
    }

    internal static BitmapSource ForNextOperation(BitmapSource decoded)
    {
        Limits.Size(decoded.PixelWidth, decoded.PixelHeight);
        if (!AcceptedDecodedDpi.Any(value => Near(decoded.DpiX, value))
            || !AcceptedDecodedDpi.Any(value => Near(decoded.DpiY, value)))
            throw new InvalidDataException(FormattableString.Invariant(
                $"Unexpected PNG/WIC density {decoded.DpiX:R}x{decoded.DpiY:R}; only 96 DPI and its 3779/3780-ppm encodings are allowed."));

        var format = decoded.Format;
        var palette = decoded.Palette;
        var originalColors = palette?.Colors.ToArray();
        var bytes = NativePixels(decoded, out var stride);
        // No format converter, drawing, resizing or alpha arithmetic occurs here.
        var working = BitmapSource.Create(decoded.PixelWidth, decoded.PixelHeight,
            WorkingDpi, WorkingDpi, format, palette, bytes, stride);
        working.Freeze();

        var verified = NativePixels(working, out var verifiedStride);
        if (working.PixelWidth != decoded.PixelWidth || working.PixelHeight != decoded.PixelHeight
            || working.Format != format || verifiedStride != stride || !bytes.AsSpan().SequenceEqual(verified)
            || !SamePalette(originalColors, working.Palette))
            throw new InvalidDataException("DPI-only normalization changed pixel dimensions, format, palette, stride or native-format bytes.");
        RequireWorkingDpi(working);
        if (!Near(working.Width, working.PixelWidth) || !Near(working.Height, working.PixelHeight))
            throw new InvalidDataException("Normalized bitmap logical bounds do not match its physical pixel dimensions.");
        Limits.Check("DPI metadata normalization");
        return working;
    }

    private static bool Near(double actual, double expected) => double.IsFinite(actual) && Math.Abs(actual - expected) <= Epsilon;

    private static bool SamePalette(Color[]? expected, BitmapPalette? actual) => expected is null
        ? actual is null
        : actual is not null && expected.SequenceEqual(actual.Colors);

    private static byte[] NativePixels(BitmapSource source, out int stride)
    {
        var bits = source.Format.BitsPerPixel;
        if (bits is < 1 or > 128) throw new InvalidDataException("Unsupported native bitmap bit depth for metadata-only copying.");
        stride = checked((source.PixelWidth * bits + 7) / 8);
        var bytes = new byte[checked(stride * source.PixelHeight)];
        source.CopyPixels(bytes, stride, 0);
        return bytes;
    }
}
