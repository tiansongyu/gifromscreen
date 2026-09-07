using System.Windows;
using System.Windows.Ink;
using System.Windows.Input;
using System.Windows.Media;

namespace GifFromScreen.CinemagraphProbe;

internal sealed record PointSpec(double X, double Y, float Pressure);
internal sealed record GeometrySpec(string Kind, double X, double Y, double Width, double Height,
    string? Tip = null, IReadOnlyList<PointSpec>? Points = null);
internal sealed record Fixture(string Id, int Width, int Height, string FirstAlpha, string CurrentAlpha,
    GeometrySpec Geometry);

internal static class Fixtures
{
    internal static IReadOnlyList<Fixture> Create()
    {
        GeometrySpec[] geometries = {
            new("empty", 0, 0, 0, 0),
            new("whole", 0, 0, 12, 10),
            new("rectangle", 2, 2, 6, 5),
            new("rectangle_fractional", 2.25, 1.5, 6.5, 5.25),
            new("ellipse", 2, 2, 7, 5),
            new("ellipse_fractional", 2.125, 1.375, 7.25, 6.5),
            new("ink_dot_ellipse", 0, 0, 4.25, 3.25, "ellipse", new[] { new PointSpec(5.125, 4.375, .6f) }),
            new("ink_dot_rectangle", 0, 0, 4.25, 3.25, "rectangle", new[] { new PointSpec(5.125, 4.375, .6f) }),
            new("ink_line_ellipse", 0, 0, 3.75, 2.25, "ellipse", new[] { new PointSpec(-1.25, 2.125, .25f), new PointSpec(10.5, 7.875, .8f) }),
            new("ink_line_rectangle", 0, 0, 3.75, 2.25, "rectangle", new[] { new PointSpec(-1.25, 2.125, .25f), new PointSpec(10.5, 7.875, .8f) }),
        };
        var fixtures = new List<Fixture>();
        foreach (var geometry in geometries)
        foreach (var first in new[] { "opaque", "zero", "partial" })
        foreach (var current in new[] { "opaque", "zero", "partial" })
            fixtures.Add(new Fixture($"{geometry.Kind}-{first}-{current}", 12, 10, first, current, geometry));
        if (fixtures.Count > 128) throw new InvalidDataException("Probe exceeds 128 fixtures.");
        return fixtures;
    }

    internal static byte[] Pixels(Fixture fixture, bool first)
    {
        Limits.Size(fixture.Width, fixture.Height);
        byte[] partial = { 0, 1, 2, 17, 63, 64, 127, 128, 160, 191, 253, 254 };
        var mode = first ? fixture.FirstAlpha : fixture.CurrentAlpha;
        var pixels = new byte[checked(fixture.Width * fixture.Height * 4)];
        for (var y = 0; y < fixture.Height; y++)
        for (var x = 0; x < fixture.Width; x++)
        {
            var index = (y * fixture.Width + x) * 4;
            var shift = first ? 0 : 93;
            pixels[index] = (byte)((17 + 37 * x + 13 * y + shift) % 256);
            pixels[index + 1] = (byte)((67 + 19 * x + 29 * y + shift) % 256);
            pixels[index + 2] = (byte)((191 + 7 * x + 31 * y + shift) % 256);
            pixels[index + 3] = mode switch {
                "opaque" => 255,
                "zero" => 0,
                "partial" => partial[(x + y * 3 + (first ? 0 : 5)) % partial.Length],
                _ => throw new InvalidDataException("Unknown alpha fixture.")
            };
        }
        return pixels;
    }

    // Editor.xaml.cs 2770-2780: GetGeometry, Union, then rectangle XOR.
    internal static Geometry OutsideClip(Fixture fixture)
    {
        var rectangle = new RectangleGeometry(new Rect(0, 0, fixture.Width, fixture.Height));
        var spec = fixture.Geometry;
        Geometry drawn;
        if (spec.Kind.StartsWith("ink_", StringComparison.Ordinal))
        {
            if (spec.Points is null || spec.Points.Count is < 1 or > 8)
                throw new InvalidDataException("Ink probe requires 1-8 explicit stylus samples.");
            var points = new StylusPointCollection(spec.Points.Select(p => new StylusPoint(p.X, p.Y, p.Pressure)));
            var stroke = new Stroke(points, new DrawingAttributes {
                Width = spec.Width, Height = spec.Height, FitToCurve = false, IgnorePressure = false,
                StylusTip = spec.Tip == "rectangle" ? StylusTip.Rectangle : StylusTip.Ellipse,
            });
            drawn = stroke.GetGeometry();
        }
        else
            drawn = spec.Kind switch {
                "empty" => Geometry.Empty,
                "whole" => rectangle,
                "rectangle" or "rectangle_fractional" => new RectangleGeometry(new Rect(spec.X, spec.Y, spec.Width, spec.Height)),
                "ellipse" or "ellipse_fractional" => new EllipseGeometry(new Rect(spec.X, spec.Y, spec.Width, spec.Height)),
                _ => throw new InvalidDataException("Unknown clip geometry.")
            };
        var union = Geometry.Combine(Geometry.Empty, drawn, GeometryCombineMode.Union, null);
        var clip = Geometry.Combine(union, rectangle, GeometryCombineMode.Xor, null);
        // Physical-pixel probe: unattached visual scale=1, image scale=1, 96 DPI.
        clip.Transform = new ScaleTransform(1, 1);
        clip.Freeze();
        return clip;
    }
}
