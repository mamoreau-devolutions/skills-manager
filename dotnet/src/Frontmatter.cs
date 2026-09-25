// Minimal YAML frontmatter parser (port of frontmatter.ts) plus a small YAML
// emitter modeled on the `yaml` package's stringify defaults, used to rewrite
// Eve SKILL.md frontmatter.
//
// YamlDotNet provides the syntax tree; plain scalars are resolved with the YAML
// 1.2 core schema, which is what the `yaml` npm package uses by default
// (`yes`/`on` stay strings, `0x1F` is a number, …).

using System.Globalization;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;
using YamlDotNet.Core;
using YamlDotNet.RepresentationModel;

namespace Skills;

internal sealed record FrontmatterResult(JsonObject Data, string Content);

internal sealed class YamlParseException(string message) : Exception(message);

internal static partial class Frontmatter
{
    [GeneratedRegex(@"\A---\r?\n([\s\S]*?)\r?\n---\r?\n?([\s\S]*)\z")]
    private static partial Regex FmRegex();

    /// Parse frontmatter. Only plain YAML `---` blocks are supported (never
    /// `---js`), so there is no code-execution path.
    /// <exception cref="YamlParseException">Invalid YAML.</exception>
    public static FrontmatterResult Parse(string raw)
    {
        var m = FmRegex().Match(raw);
        if (!m.Success) return new FrontmatterResult(new JsonObject(), raw);
        var value = ParseYaml(m.Groups[1].Value);
        return new FrontmatterResult(value as JsonObject ?? new JsonObject(), m.Groups[2].Value);
    }

    // ─── YAML → JSON ───

    /// Base of the private-use range used to smuggle non-printable characters
    /// through the YAML parser (libyaml-style parsers reject them; the `yaml`
    /// package accepts them in scalars and the CLI strips them later).
    private const int ShadowBase = 0xF0000;

    private static bool IsNonPrintable(int c) =>
        c is (>= 0x00 and <= 0x08) or 0x0B or 0x0C or (>= 0x0E and <= 0x1F) or 0x7F or (>= 0x80 and <= 0x84) or (>= 0x86 and <= 0x9F) or 0xFFFE or 0xFFFF;

    private static string Shadow(string s)
    {
        if (!s.Any(c => IsNonPrintable(c))) return s;
        var sb = new StringBuilder();
        foreach (var c in s) sb.Append(IsNonPrintable(c) ? char.ConvertFromUtf32(ShadowBase + c) : c.ToString());
        return sb.ToString();
    }

    private static string Unshadow(string s)
    {
        if (!s.Any(char.IsSurrogate)) return s;
        var sb = new StringBuilder();
        for (var i = 0; i < s.Length; i++)
        {
            if (char.IsHighSurrogate(s[i]) && i + 1 < s.Length && char.IsLowSurrogate(s[i + 1]))
            {
                var cp = char.ConvertToUtf32(s[i], s[i + 1]);
                if (cp >= ShadowBase && IsNonPrintable(cp - ShadowBase))
                {
                    sb.Append((char)(cp - ShadowBase));
                    i++;
                    continue;
                }
            }
            sb.Append(s[i]);
        }
        return sb.ToString();
    }

    public static JsonNode? ParseYaml(string src)
    {
        if (string.IsNullOrWhiteSpace(src)) return null;
        var stream = new YamlStream();
        try
        {
            stream.Load(new StringReader(Shadow(src)));
        }
        catch (YamlException e)
        {
            throw new YamlParseException(Unshadow(e.InnerException is YamlException inner && inner.Message.Length > 0 ? $"{e.Message} {inner.Message}" : e.Message));
        }
        if (stream.Documents.Count == 0) return null;
        if (stream.Documents.Count > 1) throw new YamlParseException("Source contains multiple documents; please use YAML.parseAllDocuments()");
        return ToJson(stream.Documents[0].RootNode);
    }

    [GeneratedRegex(@"\A[-+]?[0-9]+\z")]
    private static partial Regex IntRe();

    [GeneratedRegex(@"\A0o[0-7]+\z")]
    private static partial Regex OctRe();

    [GeneratedRegex(@"\A0x[0-9a-fA-F]+\z")]
    private static partial Regex HexRe();

    [GeneratedRegex(@"\A[-+]?(\.[0-9]+|[0-9]+(\.[0-9]*)?)([eE][-+]?[0-9]+)?\z")]
    private static partial Regex FloatRe();

    /// YAML 1.2 core schema resolution for a plain scalar.
    private static JsonNode? ResolvePlain(string v)
    {
        switch (v)
        {
            case "" or "~" or "null" or "Null" or "NULL": return null;
            case "true" or "True" or "TRUE": return JsonValue.Create(true);
            case "false" or "False" or "FALSE": return JsonValue.Create(false);
            case ".inf" or ".Inf" or ".INF" or "+.inf" or "+.Inf" or "+.INF": return JsonValue.Create(double.PositiveInfinity);
            case "-.inf" or "-.Inf" or "-.INF": return JsonValue.Create(double.NegativeInfinity);
            case ".nan" or ".NaN" or ".NAN": return JsonValue.Create(double.NaN);
        }
        if (IntRe().IsMatch(v)) return JsonValue.Create(double.Parse(v, CultureInfo.InvariantCulture));
        if (OctRe().IsMatch(v)) return JsonValue.Create((double)Convert.ToUInt64(v[2..], 8));
        if (HexRe().IsMatch(v)) return JsonValue.Create((double)Convert.ToUInt64(v[2..], 16));
        if (FloatRe().IsMatch(v)) return JsonValue.Create(double.Parse(v, CultureInfo.InvariantCulture));
        return JsonValue.Create(Unshadow(v));
    }

    private static JsonNode? ScalarToJson(YamlScalarNode s)
    {
        var value = s.Value ?? "";
        var tag = s.Tag.IsEmpty ? "" : s.Tag.Value;
        if (tag is "tag:yaml.org,2002:str") return JsonValue.Create(Unshadow(value));
        if (tag is "tag:yaml.org,2002:int" or "tag:yaml.org,2002:float" or "tag:yaml.org,2002:bool" or "tag:yaml.org,2002:null") return ResolvePlain(value);
        if (s.Style != ScalarStyle.Plain && s.Style != ScalarStyle.Any) return JsonValue.Create(Unshadow(value));
        return ResolvePlain(value);
    }

    private static string KeyString(YamlNode k) => ToJson(k) switch
    {
        null => "",
        JsonValue v when v.TryGetValue<string>(out var str) => str,
        JsonValue v when v.TryGetValue<bool>(out var b) => b ? "true" : "false",
        JsonValue v when v.TryGetValue<double>(out var d) => Json.NumberToString(d),
        var other => Json.Stringify(other, 0),
    };

    private static JsonNode? ToJson(YamlNode node) => node switch
    {
        YamlScalarNode s => ScalarToJson(s),
        YamlSequenceNode seq => new JsonArray(seq.Children.Select(ToJson).ToArray()),
        YamlMappingNode map => MappingToJson(map),
        _ => null,
    };

    private static JsonObject MappingToJson(YamlMappingNode map)
    {
        var obj = new JsonObject();
        foreach (var (k, v) in map.Children) obj[Unshadow(KeyString(k))] = ToJson(v);
        return Json.JsKeyOrder(obj);
    }

    // ─── YAML emitter ───

    private const int LineWidth = 80;

    private static bool IsReservedPlain(string s)
    {
        var lower = s.ToLowerInvariant();
        return lower is "true" or "false" or "null" or "~" or "yes" or "no" or "on" or "off" or ".nan" or ".inf" or "-.inf" or "+.inf"
            || double.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out _)
            || HexRe().IsMatch(s) || OctRe().IsMatch(s);
    }

    private static bool NeedsQuotes(string s)
    {
        if (s.Length == 0 || IsReservedPlain(s)) return true;
        var first = s[0];
        if ("-?:,[]{}#&*!|>'\"%@`".Contains(first))
        {
            if (first is '-' or '?' or ':')
            {
                if (s.Length < 2 || s[1] is ' ' or '\t') return true;
            }
            else
            {
                return true;
            }
        }
        if (s[0] is ' ' or '\t' || s[^1] is ' ' or '\t') return true;
        if (s.Contains(": ") || s.Contains(" #") || s.EndsWith(':')) return true;
        return s.Any(c => c < 0x20 && c != '\t');
    }

    private static string DoubleQuote(string s)
    {
        var sb = new StringBuilder("\"");
        foreach (var c in s)
        {
            switch (c)
            {
                case '"': sb.Append("\\\""); break;
                case '\\': sb.Append("\\\\"); break;
                case '\n': sb.Append("\\n"); break;
                case '\r': sb.Append("\\r"); break;
                case '\t': sb.Append("\\t"); break;
                default:
                    if (c < 0x20) sb.Append("\\x").Append(((int)c).ToString("X2", CultureInfo.InvariantCulture));
                    else sb.Append(c);
                    break;
            }
        }
        return sb.Append('"').ToString();
    }

    /// Fold a plain scalar at spaces so lines stay within LineWidth.
    private static string FoldPlain(string s, int firstCol, int indent)
    {
        if (firstCol + s.Length <= LineWidth) return s;
        var sb = new StringBuilder();
        var col = firstCol;
        var words = s.Split(' ');
        for (var i = 0; i < words.Length; i++)
        {
            var w = words[i];
            if (i == 0)
            {
                sb.Append(w);
                col += w.Length;
                continue;
            }
            if (col + 1 + w.Length > LineWidth && w.Length > 0 && col > indent)
            {
                sb.Append('\n').Append(' ', indent).Append(w);
                col = indent + w.Length;
            }
            else
            {
                sb.Append(' ').Append(w);
                col += 1 + w.Length;
            }
        }
        return sb.ToString();
    }

    private static string Scalar(JsonNode? v, int firstCol, int indent)
    {
        switch (v)
        {
            case null: return "null";
            case JsonValue jv when jv.TryGetValue<bool>(out var b): return b ? "true" : "false";
            case JsonValue jv when jv.TryGetValue<string>(out var s):
                if (s.Contains('\n'))
                {
                    var chomp = s.EndsWith('\n') ? "" : "-";
                    var body = s.TrimEnd('\n').Split('\n').Select(l => l.Length == 0 ? "" : new string(' ', indent) + l);
                    return $"|{chomp}\n{string.Join("\n", body)}";
                }
                if (NeedsQuotes(s) || s.Contains("  ")) return DoubleQuote(s);
                return FoldPlain(s, firstCol, indent);
            case JsonValue jv when Json.AsNumber(jv) is { } d:
                return double.IsNaN(d) ? ".nan" : double.IsPositiveInfinity(d) ? ".inf" : double.IsNegativeInfinity(d) ? "-.inf" : Json.NumberToString(d);
            default: return "";
        }
    }

    private static void Emit(JsonNode? v, int indent, StringBuilder sb)
    {
        switch (v)
        {
            case JsonObject map:
                foreach (var (k, val) in Json.OrderedEntries(map))
                {
                    var key = NeedsQuotes(k) ? DoubleQuote(k) : k;
                    sb.Append(' ', indent).Append(key);
                    switch (val)
                    {
                        case JsonObject { Count: > 0 }:
                        case JsonArray { Count: > 0 }:
                            sb.Append(":\n");
                            Emit(val, indent + 2, sb);
                            break;
                        case JsonObject:
                            sb.Append(": {}\n");
                            break;
                        case JsonArray:
                            sb.Append(": []\n");
                            break;
                        default:
                            sb.Append(": ").Append(Scalar(val, indent + key.Length + 2, indent + 2)).Append('\n');
                            break;
                    }
                }
                break;
            case JsonArray arr:
                foreach (var item in arr)
                {
                    sb.Append(' ', indent).Append("- ");
                    switch (item)
                    {
                        case JsonObject { Count: > 0 }:
                        case JsonArray { Count: > 0 }:
                        {
                            var nested = new StringBuilder();
                            Emit(item, indent + 2, nested);
                            sb.Append(nested.ToString().TrimStart());
                            break;
                        }
                        case JsonObject:
                            sb.Append("{}\n");
                            break;
                        case JsonArray:
                            sb.Append("[]\n");
                            break;
                        default:
                            sb.Append(Scalar(item, indent + 2, indent + 2)).Append('\n');
                            break;
                    }
                }
                break;
            default:
                sb.Append(Scalar(v, indent, indent)).Append('\n');
                break;
        }
    }

    /// `yaml.stringify(value)`
    public static string Stringify(JsonNode? v)
    {
        var sb = new StringBuilder();
        Emit(v, 0, sb);
        return sb.ToString();
    }
}
