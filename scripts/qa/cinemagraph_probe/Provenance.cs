using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Windows.Controls;
using System.Windows.Media.Imaging;

namespace GifFromScreen.CinemagraphProbe;

internal static class Provenance
{
    internal const string UpstreamCommit = "a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd";
    internal static object SourceFiles(string requested)
    {
        var root = Path.GetFullPath(requested);
        string[] names = { "Program.cs", "Fixtures.cs", "ProbeRenderer.cs", "CoverageAnalysis.cs",
            "ArtifactWriter.cs", "Provenance.cs", "CinemagraphProbe.csproj", "global.json", "run-probe.ps1", "README.md" };
        return names.Select(name => new {
            path = $"scripts/qa/cinemagraph_probe/{name}", sha256 = FileHash(Path.Combine(root, name), 256 * 1024)
        }).ToArray();
    }

    internal static object Capture()
    {
        var sdk = Environment.GetEnvironmentVariable("GFS_DOTNET_SDK_VERSION");
        var commit = Environment.GetEnvironmentVariable("GITHUB_SHA");
        if (sdk is null || !sdk.StartsWith("9.0.", StringComparison.Ordinal)) throw new InvalidOperationException("Actual .NET 9 SDK version is required.");
        if (commit is null || commit.Length != 40 || commit.Any(c => !Uri.IsHexDigit(c)))
            throw new InvalidOperationException("A full generator GITHUB_SHA is required.");
        using var process = Process.GetCurrentProcess();
        var modules = process.Modules.Cast<ProcessModule>().ToList();
        return new {
            generator_commit = commit,
            sdk_version = sdk,
            runtime = RuntimeInformation.FrameworkDescription,
            os_description = RuntimeInformation.OSDescription,
            os_version = Environment.OSVersion.VersionString,
            process_architecture = RuntimeInformation.ProcessArchitecture.ToString(),
            apartment_state = Thread.CurrentThread.GetApartmentState().ToString(),
            runner_image = new Dictionary<string, string?> {
                ["ImageOS"] = Environment.GetEnvironmentVariable("ImageOS"),
                ["ImageVersion"] = Environment.GetEnvironmentVariable("ImageVersion"),
            },
            presentation_core = Describe(typeof(BitmapSource).Assembly.Location),
            presentation_framework = Describe(typeof(Image).Assembly.Location),
            wpfgfx_cor3 = Module(modules, "wpfgfx_cor3.dll"),
            windows_codecs = Module(modules, "windowscodecs.dll"),
            generator_assembly = Describe(typeof(Program).Assembly.Location),
            captured_at_utc = DateTimeOffset.UtcNow.ToString("O"),
            peak_working_set_bytes = process.PeakWorkingSet64,
            upstream_commit = UpstreamCommit,
            source_contract = "ScreenToGif Editor.xaml.cs 2755-2808 + 5591-5628; ImageMethods.cs UIElement GetScaledRender 2104-2163. First project frame supplies the immutable reference. Clip is rectangle XOR the union of stroke.GetGeometry().",
            render_contract = "Unattached Image with Clip, Measure/Arrange, GetDescendantBounds/clamp and VisualBrush(AutoLayoutContent=false, Stretch=Fill) is the primary clip path. Its actual Pbgra32 RTB goes directly into current+clip DrawImage and final PNG/WIC. No Window, fonts, display capture, screen input or Application.Run.",
            dpi_policy = "Fixed physical-pixel 96-DPI space, scale=1/image-scale=1. PNG decoded DPI is recorded. Working bitmap DPI alone is reset to 96 after asserting native-format pixels and palette unchanged. This does not claim coverage of unnormalized fractional-DPI or attached-window behavior.",
            model_contract = "A8 multiplication, 8x8 coverage(count 0..64, scale=count*4) and source-over models are diagnostic hypotheses. Actual clip/final outputs always come from WPF. No Rust implementation or previous reference-generator inputs are used or modified.",
        };
    }

    private static object Module(IEnumerable<ProcessModule> modules, string name)
    {
        var module = modules.FirstOrDefault(m => string.Equals(m.ModuleName, name, StringComparison.OrdinalIgnoreCase))
            ?? throw new InvalidOperationException($"Required WPF/WIC module was not loaded: {name}");
        return Describe(module.FileName);
    }

    private static object Describe(string path)
    {
        var version = FileVersionInfo.GetVersionInfo(path).FileVersion;
        if (string.IsNullOrEmpty(version)) throw new InvalidOperationException($"No version for provenance binary {Path.GetFileName(path)}.");
        return new { path, file_version = version, sha256 = FileHash(path, 128 * 1024 * 1024) };
    }

    private static string FileHash(string path, long maximum)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.ReadWrite | FileShare.Delete);
        if (stream.Length > maximum) throw new InvalidDataException($"Provenance file too large: {Path.GetFileName(path)}.");
        return Convert.ToHexString(SHA256.HashData(stream)).ToLowerInvariant();
    }
}
