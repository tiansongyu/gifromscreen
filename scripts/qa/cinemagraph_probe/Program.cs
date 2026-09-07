global using System.IO;

using System.Diagnostics;
using System.Globalization;
using System.Text.Json;
using System.Windows.Media;

namespace GifFromScreen.CinemagraphProbe;

internal static class Program
{
    internal static readonly JsonSerializerOptions Json = new() {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower, WriteIndented = true,
    };

    [STAThread]
    private static int Main(string[] args)
    {
        try
        {
            if (args.Length != 2) throw new ArgumentException("Usage: CinemagraphProbe <source-directory> <new-output-directory>");
            if (!OperatingSystem.IsWindows() || Environment.Version.Major != 9 ||
                Thread.CurrentThread.GetApartmentState() != ApartmentState.STA)
                throw new InvalidOperationException("Probe requires real Windows .NET 9 WPF and an STA entry point.");
            CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
            CultureInfo.CurrentUICulture = CultureInfo.InvariantCulture;
            var sourceFiles = Provenance.SourceFiles(args[0]);
            var fixtures = Fixtures.Create();
            var writer = new ArtifactWriter(args[1]);
            var results = new List<object>();
            foreach (var fixture in fixtures)
            {
                Limits.Check(fixture.Id);
                results.Add(Run(fixture, writer));
            }
            writer.Write("index.json", "json", JsonSerializer.SerializeToUtf8Bytes(new {
                format_version = 1, fixture_count = fixtures.Count, definition_sha256 = ArtifactWriter.Hash(JsonSerializer.SerializeToUtf8Bytes(fixtures, Json)),
                provenance = Provenance.Capture(), source_files = sourceFiles, cases = results,
                interpretation = "Differences are measured findings, not test failures. Neither an A8 nor coverage64 hypothesis is assumed to represent real Image.Clip exactly. Only direct clip RTB -> Overlay -> final PNG is the primary upstream path.",
            }, Json));
            Console.WriteLine($"PROBE_COMPLETE cases={results.Count} artifact_bytes={writer.TotalBytes}");
            return 0;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"Cinemagraph probe failed: {error.GetType().Name}: {error.Message}");
            Console.Error.WriteLine("New partial artifacts remain diagnostic only. No existing output was overwritten.");
            return 1;
        }
    }

    private static object Run(Fixture fixture, ArtifactWriter writer)
    {
        var artifacts = new List<Artifact>();
        void Save(string name, string format, byte[] bytes) => artifacts.Add(writer.Write($"{fixture.Id}/{name}", format, bytes));
        var firstRequested = Fixtures.Pixels(fixture, first: true);
        var currentRequested = Fixtures.Pixels(fixture, first: false);
        Save("first-requested.rgba", "straight-rgba8", firstRequested);
        Save("current-requested.rgba", "straight-rgba8", currentRequested);
        // Project frames are PNGs. These are initialization boundaries, not a
        // hidden extra boundary between the clipped RTB and OverlayAsync.
        var first = ProbeRenderer.PngRoundTrip(ProbeRenderer.Create(fixture.Width, fixture.Height, firstRequested));
        var current = ProbeRenderer.PngRoundTrip(ProbeRenderer.Create(fixture.Width, fixture.Height, currentRequested));
        Save("first.png", "png", first.Png);
        Save("current.png", "png", current.Png);
        Save("first-initial.bgra", "straight-bgra8", ProbeRenderer.Pixels(first.Working, PixelFormats.Bgra32));
        Save("current-initial.bgra", "straight-bgra8", ProbeRenderer.Pixels(current.Working, PixelFormats.Bgra32));
        var firstPm = ProbeRenderer.Pixels(first.Working, PixelFormats.Pbgra32);
        var currentPm = ProbeRenderer.Pixels(current.Working, PixelFormats.Pbgra32);
        Save("first-initial.pbgra", "premultiplied-bgra8", firstPm);
        Save("current-initial.pbgra", "premultiplied-bgra8", currentPm);
        var geometry = Fixtures.OutsideClip(fixture);
        var clipped = ProbeRenderer.ClipImage(first.Working, geometry);
        var clippedPm = ProbeRenderer.Pixels(clipped.Bitmap, PixelFormats.Pbgra32);
        Save("clip-direct.pbgra", "premultiplied-bgra8", clippedPm);
        var white = ProbeRenderer.Create(fixture.Width, fixture.Height, Enumerable.Repeat((byte)255, firstPm.Length).ToArray());
        var whiteClip = ProbeRenderer.ClipImage(white, geometry);
        var whitePm = ProbeRenderer.Pixels(whiteClip.Bitmap, PixelFormats.Pbgra32);
        Save("clip-white.pbgra", "premultiplied-bgra8", whitePm);
        var coverage = CoverageAnalysis.Analyze(firstPm, whitePm, clippedPm, fixture.Width);
        Save("coverage.a8", "white-clip-alpha8", coverage.Alpha8);
        Save("coverage.c64", "count-0-through-64-or-255-if-white-alpha-not-representable", coverage.Count64);
        Save("mask8-predicted.pbgra", "diagnostic-premultiplied-bgra8", coverage.Mask8Prediction);
        Save("coverage64-predicted.pbgra", "diagnostic-premultiplied-bgra8", coverage.Coverage64Prediction);
        var direct = ProbeRenderer.Overlay(current.Working, clipped.Bitmap);
        var directPm = ProbeRenderer.Pixels(direct, PixelFormats.Pbgra32);
        var final = ProbeRenderer.PngRoundTrip(direct);
        Save("direct-final.pbgra", "premultiplied-bgra8", directPm);
        Save("direct-final.png", "png", final.Png);
        Save("direct-final.rgba", "straight-rgba8", final.Rgba);
        var model = CoverageAnalysis.SourceOver(clippedPm, currentPm);
        Save("source-over-model.pbgra", "diagnostic-premultiplied-bgra8", model);
        // Deliberately wrong extra-boundary candidate, isolated from the main result.
        var extraClip = ProbeRenderer.PngRoundTrip(clipped.Bitmap);
        var extraClipPm = ProbeRenderer.Pixels(extraClip.Working, PixelFormats.Pbgra32);
        var extraFinalRtb = ProbeRenderer.Overlay(current.Working, extraClip.Working);
        var extraFinalPm = ProbeRenderer.Pixels(extraFinalRtb, PixelFormats.Pbgra32);
        var extraFinal = ProbeRenderer.PngRoundTrip(extraFinalRtb);
        Save("extra-clip.png", "png", extraClip.Png);
        Save("extra-clip.pbgra", "premultiplied-bgra8-after-extra-png", extraClipPm);
        Save("extra-final.pbgra", "premultiplied-bgra8", extraFinalPm);
        Save("extra-final.png", "png", extraFinal.Png);
        Save("extra-final.rgba", "straight-rgba8", extraFinal.Rgba);
        var shortcut = ProbeRenderer.Pixels(ProbeRenderer.PushClipDiagnostic(first.Working, geometry), PixelFormats.Pbgra32);
        Save("pushclip-diagnostic.pbgra", "premultiplied-bgra8", shortcut);
        var path = geometry.ToString(CultureInfo.InvariantCulture);
        if (path.Length > 65_536) throw new InvalidDataException("Geometry diagnostic exceeds 64 KiB.");
        var directModel = CoverageAnalysis.Compare(model, directPm, fixture.Width);
        var extraPixels = CoverageAnalysis.Compare(final.Rgba, extraFinal.Rgba, fixture.Width);
        Console.WriteLine($"{fixture.Id}: mask8={coverage.Report.Mask8FromWhite.DifferentPixels} c64={coverage.Report.Coverage64FromWhite.DifferentPixels} directpm={directModel.DifferentPixels} extra_png={extraPixels.DifferentPixels}");
        return new {
            fixture, artifacts, clipped.DescendantBounds, clipped.UsedBounds, geometry = path,
            white_descendant_bounds = whiteClip.DescendantBounds, white_used_bounds = whiteClip.UsedBounds,
            coverage = coverage.Report, direct_source_over_model = directModel,
            extra_png_clip = CoverageAnalysis.Compare(clippedPm, extraClipPm, fixture.Width),
            extra_png_final_pm = CoverageAnalysis.Compare(directPm, extraFinalPm, fixture.Width),
            extra_png_final_rgba = extraPixels,
            pushclip_vs_image_visualbrush = CoverageAnalysis.Compare(clippedPm, shortcut, fixture.Width),
            decoded_dpi = new { first = new[] { first.DecodedDpiX, first.DecodedDpiY }, current = new[] { current.DecodedDpiX, current.DecodedDpiY },
                final = new[] { final.DecodedDpiX, final.DecodedDpiY }, extra_clip = new[] { extraClip.DecodedDpiX, extraClip.DecodedDpiY },
                extra_final = new[] { extraFinal.DecodedDpiX, extraFinal.DecodedDpiY } },
        };
    }
}

internal static class Limits
{
    internal const long MaxOutputBytes = 16 * 1024 * 1024;
    private static readonly Stopwatch Elapsed = Stopwatch.StartNew();
    internal static void Size(int width, int height)
    {
        if (width is < 1 or > 32 || height is < 1 or > 32) throw new InvalidDataException("Probe dimensions must be 1..32.");
    }
    internal static void Check(string phase)
    {
        using var process = Process.GetCurrentProcess();
        if (process.WorkingSet64 > 512L * 1024 * 1024) throw new InvalidOperationException($"512 MiB limit exceeded at {phase}.");
        if (Elapsed.Elapsed > TimeSpan.FromSeconds(115)) throw new TimeoutException($"Probe phase deadline exceeded at {phase}.");
    }
}
