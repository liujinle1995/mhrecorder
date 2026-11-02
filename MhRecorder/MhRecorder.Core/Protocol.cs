// Wire protocol mirror of `src/crates/mhcore/src/protocol.rs`.
// One request/response per line, UTF-8 JSON, newline delimited.

using System.Text.Json;
using System.Text.Json.Serialization;

namespace MhRecorder.Core;

public static class ProtocolConstants
{
    public const string PipeName = "mhrecorder.v1";
    public const int Version = 1;
    public const string AppVersion = "0.10.0";
}

/// <summary>Canonical method names (mirror of `protocol::method`).</summary>
public static class Methods
{
    public const string Ping = "ping";
    public const string Version = "version";
    public const string Recognize = "recognize";
    public const string RecognizeSave = "recognize.save";
    public const string CharactersList = "characters.list";
    public const string CharactersGet = "characters.get";
    public const string CharactersUpdate = "characters.update";
    public const string CharactersDelete = "characters.delete";
    public const string CharactersConfirm = "characters.confirm";
    public const string Statistics = "statistics.get";
    public const string SettingsGet = "settings.get";
    public const string SettingsSet = "settings.set";
    public const string ExportJson = "export.json";
    public const string ExportCsv = "export.csv";
    public const string ExportZip = "export.zip";
    public const string DataClear = "data.clear";
}

/// <summary>Error codes the client switch-cases on (mirror of `protocol::errcode`).</summary>
public static class ErrorCodes
{
    public const string BadRequest = "badRequest";
    public const string NotFound = "notFound";
    public const string Decode = "decodeFailed";
    public const string Ocr = "ocrFailed";
    public const string Storage = "storageFailed";
    public const string Io = "ioFailed";
    public const string Internal = "internal";
}

public sealed class WireRequest
{
    public long Id { get; set; }
    public string Method { get; set; } = string.Empty;
    public object? Params { get; set; }
}

public sealed class WireError
{
    public string Code { get; set; } = string.Empty;
    public string Message { get; set; } = string.Empty;
}

public sealed class WireResponse
{
    public long Id { get; set; }
    public bool Ok { get; set; }
    public JsonElement? Result { get; set; }
    public WireError? Error { get; set; }
}

/// <summary>The exception raised when the backend answers `ok:false`.</summary>
public sealed class BackendException : Exception
{
    public BackendException(string code, string message) : base(message)
    {
        Code = code;
    }

    public string Code { get; }
}

/// <summary>Raised when no backend is reachable over the named pipe.</summary>
public sealed class BackendUnavailableException : Exception
{
    public BackendUnavailableException(string message) : base(message)
    {
    }
}

/// <summary>Shared serializer settings: camelCase everywhere, nulls omitted.</summary>
public static class Json
{
    public static readonly JsonSerializerOptions Options = Create();

    private static JsonSerializerOptions Create()
    {
        var options = new JsonSerializerOptions
        {
            PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
            PropertyNameCaseInsensitive = true,
            DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
            NumberHandling = JsonNumberHandling.AllowReadingFromString,
        };
        options.Converters.Add(new JsonStringEnumConverter(JsonNamingPolicy.CamelCase));
        return options;
    }

    public static string Serialize<T>(T value) => JsonSerializer.Serialize(value, Options);

    public static T? Deserialize<T>(string json) => JsonSerializer.Deserialize<T>(json, Options);

    public static T? DeserializeElement<T>(JsonElement element) =>
        JsonSerializer.Deserialize<T>(element.GetRawText(), Options);
}
