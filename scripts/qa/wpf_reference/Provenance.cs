using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Windows.Media.Imaging;

namespace GifFromScreen.WpfReference;

internal sealed record GeneratorFile(string Path, string Sha256);
internal sealed record BinaryFile(string Path, string FileVersion, string Sha256);

internal static class Provenance
{
    internal static IReadOnlyList<GeneratorFile> GeneratorFiles(string definitionDirectory)
    {
        var names = Directory.EnumerateFiles(definitionDirectory, "*.cs", SearchOption.TopDirectoryOnly)
            .Select(path => Path.GetFileName(path) ?? throw new InvalidDataException("Missing generator source name."))
            .OrderBy(name => name, StringComparer.Ordinal).ToList();
        if (names.Count is < 1 or > 16 || !names.Contains("Program.cs", StringComparer.Ordinal))
            throw new InvalidDataException("Definition directory must contain the bounded top-level generator sources.");
        names.Add("WpfReference.csproj");
        names.Add("global.json");
        return names.Select(name => new GeneratorFile($"scripts/qa/wpf_reference/{name}",
            Hashing.File(Path.Combine(definitionDirectory, name), 256 * 1024))).ToList();
    }

    internal static object Capture()
    {
        var sdk = Environment.GetEnvironmentVariable("GFS_DOTNET_SDK_VERSION");
        if (sdk is null || !sdk.StartsWith("9.0.", StringComparison.Ordinal))
            throw new InvalidOperationException("GFS_DOTNET_SDK_VERSION must record the actual selected .NET 9 SDK.");
        var commit = Environment.GetEnvironmentVariable("GITHUB_SHA");
        if (string.IsNullOrEmpty(commit)) throw new InvalidOperationException("GITHUB_SHA is required for reference provenance.");
        using var process = Process.GetCurrentProcess();
        var nativeModules = process.Modules.Cast<ProcessModule>().ToList();
        var presentation = Describe(typeof(BitmapSource).Assembly.Location);
        var renderer = Module(nativeModules, "wpfgfx_cor3.dll");
        var codecs = Module(nativeModules, "windowscodecs.dll");
        // Deliberate allowlist: never dump the runner environment or tokens.
        return new
        {
            sdk_version = sdk,
            runtime = RuntimeInformation.FrameworkDescription,
            os_description = RuntimeInformation.OSDescription,
            os_version = Environment.OSVersion.VersionString,
            process_architecture = RuntimeInformation.ProcessArchitecture.ToString(),
            generator_commit = commit,
            runner_image = new Dictionary<string, string?>
            {
                ["ImageOS"] = Environment.GetEnvironmentVariable("ImageOS"),
                ["ImageVersion"] = Environment.GetEnvironmentVariable("ImageVersion"),
            },
            presentation_core = presentation,
            wpfgfx_cor3 = renderer,
            windows_codecs = codecs,
            generator_assembly = Describe(typeof(Program).Assembly.Location),
            apartment_state = Thread.CurrentThread.GetApartmentState().ToString(),
            captured_at_utc = DateTimeOffset.UtcNow.ToString("O"),
            peak_working_set_bytes = process.PeakWorkingSet64,
            source_contract = "ScreenToGif a4d0a67c2131cd048ceec86cd40afc2f1a06f2fd, Editor BorderAsync/ShadowAsync, 96 DPI",
            runtime_contract = "Actual installed .NET 9 WPF RenderTargetBitmap + PNG/WIC; not simulated Rust pixels",
        };
    }

    private static BinaryFile Module(IEnumerable<ProcessModule> modules, string name)
    {
        var module = modules.FirstOrDefault(module => string.Equals(module.ModuleName, name, StringComparison.OrdinalIgnoreCase));
        if (module is null) throw new InvalidOperationException($"Required reference provenance module was not loaded: {name}");
        return Describe(module.FileName);
    }

    private static BinaryFile Describe(string path)
    {
        var version = FileVersionInfo.GetVersionInfo(path).FileVersion;
        if (string.IsNullOrEmpty(version)) throw new InvalidOperationException($"Reference file has no version information: {Path.GetFileName(path)}");
        return new BinaryFile(path, version, Hashing.File(path, 128 * 1024 * 1024));
    }
}
