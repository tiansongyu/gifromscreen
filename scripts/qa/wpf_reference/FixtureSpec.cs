using System.Text.Json;
using System.Text.RegularExpressions;
using System.Windows.Media;

namespace GifFromScreen.WpfReference;

internal sealed record ImageSpec(int Width, int Height, byte[] Rgba);
internal sealed record Fixture(string Id, ImageSpec Source, IReadOnlyList<Operation> Operations);
internal abstract record Operation;
internal sealed record BorderOperation(BorderStyle Style) : Operation;
internal sealed record ShadowOperation(ShadowStyle Style) : Operation;
internal sealed record OverlayOperation(int X, int Y, ImageSpec Image) : Operation;
internal sealed record VectorShapesOperation(IReadOnlyList<VectorShapeSpec> Shapes) : Operation;
internal enum VectorKind { Rectangle, Ellipse, Triangle, BlockArrow }
internal readonly record struct VectorBounds(long XHundredths, long YHundredths, long WidthHundredths, long HeightHundredths);
internal sealed record VectorShapeSpec(int Version, VectorKind Kind, VectorBounds Bounds,
    int StrokeWidthHundredths, Rgba Stroke, Rgba? Fill, int CornerRadiusHundredths, int RotationHundredths);
internal readonly record struct Rgba(byte Red, byte Green, byte Blue, byte Alpha)
{
    internal Color ToColor() => Color.FromArgb(Alpha, Red, Green, Blue);
}
internal readonly record struct Edges(int TopMilli, int RightMilli, int BottomMilli, int LeftMilli);
internal readonly record struct BorderStyle(Edges Widths, Rgba Color, Rgba Background);
internal readonly record struct ShadowStyle(int BlurRadiusHundredths, int DepthHundredths,
    int DirectionHundredths, int OpacityBasisPoints, Rgba Color, Rgba Background);

internal static class FixtureParser
{
    internal static IReadOnlyList<Fixture> Parse(byte[] bytes)
    {
        using var document = JsonDocument.Parse(bytes, new JsonDocumentOptions { MaxDepth = 16 });
        var root = Object(document.RootElement, "format_version", "fixtures");
        if (Integer(root, "format_version", 1, 1) != 1)
            throw new InvalidDataException("Unsupported definition format.");
        var array = Array(root.GetProperty("fixtures"), Limits.FixtureCount, Limits.FixtureCount);
        var ids = new HashSet<string>(StringComparer.Ordinal);
        var fixtures = new List<Fixture>();
        foreach (var entry in array.EnumerateArray())
        {
            var item = Object(entry, "id", "source", "operations");
            var id = item.GetProperty("id").GetString() ?? throw new InvalidDataException("Missing fixture id.");
            if (!Regex.IsMatch(id, "\\A[a-z0-9-]{1,64}\\z", RegexOptions.CultureInvariant)
                || IsReservedDevice(id) || !ids.Add(id))
                throw new InvalidDataException("Fixture ids must be unique portable ASCII names: [-a-z0-9], length 1..64, not Windows device names.");
            var source = Image(item.GetProperty("source"));
            var operations = Array(item.GetProperty("operations"), 1, 8)
                .EnumerateArray().Select(ParseOperation).ToList();
            fixtures.Add(new Fixture(id, source, operations));
        }
        return fixtures;
    }

    private static bool IsReservedDevice(string id) => id is "con" or "prn" or "aux" or "nul"
        || Regex.IsMatch(id, "\\A(?:com|lpt)[1-9]\\z", RegexOptions.CultureInvariant);

    private static Operation ParseOperation(JsonElement element)
    {
        if (element.ValueKind != JsonValueKind.Object)
            throw new InvalidDataException("Operation must be an object.");
        return element.GetProperty("kind").GetString() switch
        {
            "image_border" => new BorderOperation(Border(Object(element, "kind", "style").GetProperty("style"))),
            "image_shadow" => new ShadowOperation(Shadow(Object(element, "kind", "style").GetProperty("style"))),
            "overlay" => Overlay(Object(element, "kind", "x", "y", "image")),
            "vector_shapes" => new VectorShapesOperation(Array(Object(element, "kind", "shapes").GetProperty("shapes"), 1, Limits.MaxShapes)
                .EnumerateArray().Select(VectorShape).ToList()),
            _ => throw new InvalidDataException("Unknown operation kind; no executable or external-image operations are permitted."),
        };
    }

    private static OverlayOperation Overlay(JsonElement element) => new(
        Integer(element, "x", 0, Limits.MaxDimension), Integer(element, "y", 0, Limits.MaxDimension), Image(element.GetProperty("image")));

    private static VectorShapeSpec VectorShape(JsonElement element)
    {
        var shape = Object(element, "version", "kind", "bounds", "stroke_width_hundredths", "stroke", "fill", "corner_radius_hundredths", "rotation_hundredths");
        var kind = shape.GetProperty("kind").GetString() switch
        {
            "rectangle" => VectorKind.Rectangle,
            "ellipse" => VectorKind.Ellipse,
            "triangle" => VectorKind.Triangle,
            "block_arrow" => VectorKind.BlockArrow,
            _ => throw new InvalidDataException("Unsupported vector-shape kind."),
        };
        var bounds = Object(shape.GetProperty("bounds"), "x_hundredths", "y_hundredths", "width_hundredths", "height_hundredths");
        const int extent = 13_107_000; // Same declared twice-GIF-axis metadata bound.
        var rectangle = new VectorBounds(Integer(bounds, "x_hundredths", -extent, extent),
            Integer(bounds, "y_hundredths", -extent, extent),
            Integer(bounds, "width_hundredths", 1, extent), Integer(bounds, "height_hundredths", 1, extent));
        var right = checked(rectangle.XHundredths + rectangle.WidthHundredths);
        var bottom = checked(rectangle.YHundredths + rectangle.HeightHundredths);
        if (right < -extent || right > extent || bottom < -extent || bottom > extent)
            throw new InvalidDataException("Vector-shape checked ends exceed the declared coordinate range.");
        var fill = shape.GetProperty("fill");
        return new VectorShapeSpec(Integer(shape, "version", 1, 1), kind, rectangle,
            Integer(shape, "stroke_width_hundredths", 0, 10_000), Color(shape.GetProperty("stroke")),
            fill.ValueKind == JsonValueKind.Null ? null : Color(fill),
            Integer(shape, "corner_radius_hundredths", 0, 10_000),
            Integer(shape, "rotation_hundredths", 0, 35_999));
    }

    private static ImageSpec Image(JsonElement element)
    {
        var image = Object(element, "width", "height", "pixels");
        var width = Integer(image, "width", 1, Limits.MaxDimension);
        var height = Integer(image, "height", 1, Limits.MaxDimension);
        var count = checked(width * height);
        var pixels = Array(image.GetProperty("pixels"), count, count);
        var rgba = new byte[checked(count * 4)];
        var offset = 0;
        foreach (var pixel in pixels.EnumerateArray())
            foreach (var channel in Array(pixel, 4, 4).EnumerateArray())
            {
                if (!channel.TryGetByte(out var value))
                    throw new InvalidDataException("Every RGBA channel must be an integer in 0..255.");
                rgba[offset++] = value;
            }
        return new ImageSpec(width, height, rgba);
    }

    private static BorderStyle Border(JsonElement element)
    {
        var style = Object(element, "widths", "color", "background");
        var widths = Object(style.GetProperty("widths"), "top_milli", "right_milli", "bottom_milli", "left_milli");
        var result = new BorderStyle(new Edges(
            Integer(widths, "top_milli", -500_000, 50_000), Integer(widths, "right_milli", -500_000, 50_000),
            Integer(widths, "bottom_milli", -500_000, 50_000), Integer(widths, "left_milli", -500_000, 50_000)),
            Color(style.GetProperty("color")), Color(style.GetProperty("background")));
        if (result.Background != new Rgba(255, 255, 255, 255))
            throw new InvalidDataException("Pinned ScreenToGif Border Apply has an opaque white background; the reference must not silently substitute a different one.");
        return result;
    }

    private static ShadowStyle Shadow(JsonElement element)
    {
        var style = Object(element, "blur_radius_hundredths", "depth_hundredths", "direction_hundredths", "opacity_basis_points", "color", "background");
        return new ShadowStyle(
            Integer(style, "blur_radius_hundredths", 0, 10_000), Integer(style, "depth_hundredths", 0, 10_000),
            Integer(style, "direction_hundredths", 0, 36_000), Integer(style, "opacity_basis_points", 0, 10_000),
            Color(style.GetProperty("color")), Color(style.GetProperty("background")));
    }

    private static Rgba Color(JsonElement element)
    {
        var color = Object(element, "red", "green", "blue", "alpha");
        return new Rgba((byte)Integer(color, "red", 0, 255), (byte)Integer(color, "green", 0, 255),
            (byte)Integer(color, "blue", 0, 255), (byte)Integer(color, "alpha", 0, 255));
    }

    private static int Integer(JsonElement parent, string name, int minimum, int maximum)
    {
        if (!parent.GetProperty(name).TryGetInt32(out var value) || value < minimum || value > maximum)
            throw new InvalidDataException($"{name} must be an integer in {minimum}..{maximum}.");
        return value;
    }

    private static JsonElement Array(JsonElement element, int minimum, int maximum)
    {
        if (element.ValueKind != JsonValueKind.Array || element.GetArrayLength() < minimum || element.GetArrayLength() > maximum)
            throw new InvalidDataException($"Expected an array containing {minimum}..{maximum} entries.");
        return element;
    }

    private static JsonElement Object(JsonElement element, params string[] names)
    {
        if (element.ValueKind != JsonValueKind.Object) throw new InvalidDataException("Expected an object.");
        var remaining = new HashSet<string>(names, StringComparer.Ordinal);
        foreach (var property in element.EnumerateObject())
            if (!remaining.Remove(property.Name)) throw new InvalidDataException($"Unexpected or duplicate property: {property.Name}.");
        if (remaining.Count != 0) throw new InvalidDataException($"Missing properties: {string.Join(", ", remaining)}.");
        return element;
    }
}
