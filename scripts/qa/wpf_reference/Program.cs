// The desktop SDK's implicit usings differ from the console SDK's set.
global using System.IO;

using System.Diagnostics;
using System.Globalization;
using System.Text.Json;

namespace GifFromScreen.WpfReference;

internal static class Program
{
    [STAThread]
    private static int Main(string[] args)
    {
        try
        {
            if (args.Length != 2)
                throw new ArgumentException("Usage: WpfReference <fixtures.json> <new-output-directory>");
            if (!OperatingSystem.IsWindows() || Environment.Version.Major != 9)
                throw new InvalidOperationException("This reference requires Windows and the .NET 9 runtime.");
            if (Thread.CurrentThread.GetApartmentState() != ApartmentState.STA)
                throw new InvalidOperationException("The reference entry point must run synchronously in STA.");

            var definitionPath = Path.GetFullPath(args[0]);
            var bytes = ReadDefinition(definitionPath);
            var fixtures = FixtureParser.Parse(bytes);
            // Validate every operation's geometry before creating files or WPF bitmaps.
            foreach (var fixture in fixtures)
            {
                var canvas = new PixelSize(fixture.Source.Width, fixture.Source.Height);
                foreach (var operation in fixture.Operations)
                    canvas = WpfRenderer.OutputSize(canvas, operation);
            }
            var generatorFiles = Provenance.GeneratorFiles(Path.GetDirectoryName(definitionPath)!);
            Limits.Check("validated inputs");
            var writer = new ArtifactWriter(args[1]);
            var results = new List<FixtureArtifacts>();
            foreach (var fixture in fixtures)
            {
                Limits.Check($"{fixture.Id}: input");
                var initial = writer.Snapshot(fixture.Id, "input", WpfRenderer.CreateBitmap(fixture.Source));
                var current = initial.Working;
                var stages = new List<BitmapArtifact>();
                for (var index = 0; index < fixture.Operations.Count; index++)
                {
                    Limits.Check($"{fixture.Id}: before stage {index + 1}");
                    var rendered = WpfRenderer.Apply(current, fixture.Operations[index]);
                    var stageName = "stage-" + (index + 1).ToString("00", CultureInfo.InvariantCulture);
                    var snapshot = writer.Snapshot(fixture.Id, stageName, rendered);
                    current = snapshot.Working;
                    stages.Add(snapshot.Artifact);
                    Limits.Check($"{fixture.Id}: after stage {index + 1}");
                }
                results.Add(new FixtureArtifacts(fixture.Id, initial.Artifact, stages));
                Console.WriteLine($"{fixture.Id}: {stages.Count} stages, final {current.PixelWidth}x{current.PixelHeight}");
            }
            var indexDocument = new ReferenceIndex(1, Hashing.Bytes(bytes), Provenance.Capture(), generatorFiles, results);
            writer.Write("index.json", JsonSerializer.SerializeToUtf8Bytes(indexDocument, JsonOptions));
            Limits.Check("complete");
            Console.WriteLine($"Reference complete: {results.Count} fixtures, {writer.TotalBytes} artifact bytes.");
            return 0;
        }
        catch (Exception error)
        {
            Console.Error.WriteLine($"WPF reference failed: {error.GetType().Name}: {error.Message}");
            Console.Error.WriteLine("No existing output was overwritten. Any new partial artifacts are retained for diagnosis; they are not a completed reference.");
            return 1;
        }
    }

    internal static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        WriteIndented = true,
    };

    private static byte[] ReadDefinition(string path)
    {
        using var stream = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
        var buffer = new byte[Limits.MaxDefinitionBytes + 1];
        var count = 0;
        while (count < buffer.Length)
        {
            var read = stream.Read(buffer, count, buffer.Length - count);
            if (read == 0) break;
            count += read;
        }
        if (count == 0 || count > Limits.MaxDefinitionBytes)
            throw new InvalidDataException("Definition must contain 1 through 65,536 bytes.");
        return buffer[..count];
    }
}

internal static class Limits
{
    internal const int MaxDefinitionBytes = 64 * 1024;
    internal const int MaxDimension = 256;
    internal const int FixtureCount = 15;
    internal const int MaxShapes = 16;
    internal const long MaxOutputBytes = 16 * 1024 * 1024;
    private const long MaxWorkingSet = 512L * 1024 * 1024;
    private static readonly Stopwatch Elapsed = Stopwatch.StartNew();

    internal static void Check(string phase)
    {
        using var process = Process.GetCurrentProcess();
        if (process.WorkingSet64 > MaxWorkingSet)
            throw new InvalidOperationException($"Working set exceeds 512 MiB at {phase}.");
        if (Elapsed.Elapsed > TimeSpan.FromSeconds(115))
            throw new TimeoutException($"Reference exceeded its phase deadline at {phase}.");
    }

    internal static PixelSize Size(int width, int height)
    {
        if (width < 1 || height < 1 || width > MaxDimension || height > MaxDimension)
            throw new InvalidDataException("Every input and rendered stage must be between 1x1 and 256x256.");
        return new PixelSize(width, height);
    }
}

internal sealed record ReferenceIndex(int FormatVersion, string DefinitionSha256, object Provenance,
    IReadOnlyList<GeneratorFile> GeneratorFiles, IReadOnlyList<FixtureArtifacts> Fixtures);
internal sealed record FixtureArtifacts(string Id, BitmapArtifact Input, IReadOnlyList<BitmapArtifact> Stages);
internal readonly record struct PixelSize(int Width, int Height);
