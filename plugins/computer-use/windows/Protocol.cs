using System;
using System.Collections.Generic;
using System.Globalization;
using System.Security.Cryptography;
using System.Text;
using System.Web.Script.Serialization;

namespace OpenHarness.ComputerUse {
    internal sealed class ComputerError : Exception {
        internal readonly string Code;
        internal ComputerError(string code, string message) : base(message) { Code = code; }
    }

    internal static class Json {
        internal static readonly JavaScriptSerializer Serializer = new JavaScriptSerializer {
            MaxJsonLength = 6 * 1024 * 1024, RecursionLimit = 32
        };
        internal static Dictionary<string, object> Object(params object[] entries) {
            var value = new Dictionary<string, object>();
            for (int i = 0; i < entries.Length; i += 2) value.Add((string)entries[i], entries[i + 1]);
            return value;
        }
        internal static string Encode(object value) { return Serializer.Serialize(value); }
        internal static string Hash(string value) {
            using (var sha = SHA256.Create()) return BitConverter.ToString(sha.ComputeHash(Encoding.UTF8.GetBytes(value))).Replace("-", "").ToLowerInvariant();
        }
        internal static string Text(string value, int limit) {
            if (value == null) return "";
            var result = new StringBuilder();
            for (int i = 0; i < value.Length && result.Length < limit; i++) {
                char c = value[i];
                if (char.IsHighSurrogate(c) && i + 1 < value.Length && char.IsLowSurrogate(value[i + 1])) {
                    if (result.Length + 2 > limit) break;
                    result.Append(c); result.Append(value[++i]);
                } else result.Append(c == '\0' || char.IsSurrogate(c) ? '\uFFFD' : c);
            }
            return result.ToString();
        }
    }

    internal static class Arguments {
        internal static void Keys(Dictionary<string, object> values, params string[] allowed) {
            var keys = new HashSet<string>(allowed, StringComparer.Ordinal);
            foreach (string key in values.Keys) if (!keys.Contains(key)) throw Invalid();
        }
        internal static ComputerError Invalid() { return new ComputerError("INVALID_REQUEST", "Invalid computer helper request."); }
        internal static string String(Dictionary<string, object> values, string key, int maximum) { return String(values, key, maximum, false); }
        internal static string String(Dictionary<string, object> values, string key, int maximum, bool allowEmpty) {
            object raw;
            if (!values.TryGetValue(key, out raw) || !(raw is string)) throw Invalid();
            var value = (string)raw;
            if ((!allowEmpty && value.Length == 0) || value.Length > maximum || value.IndexOf('\0') >= 0 || Json.Text(value, maximum) != value) throw Invalid();
            return value;
        }
        internal static double Number(Dictionary<string, object> values, string key) {
            object raw;
            if (!values.TryGetValue(key, out raw) || !(raw is int || raw is long || raw is double || raw is decimal)) throw Invalid();
            double value = Convert.ToDouble(raw, CultureInfo.InvariantCulture);
            if (double.IsNaN(value) || double.IsInfinity(value)) throw Invalid();
            return value;
        }
        internal static int Integer(Dictionary<string, object> values, string key, int minimum, int maximum) {
            double value = Number(values, key);
            if (value != Math.Truncate(value) || value < minimum || value > maximum) throw Invalid();
            return (int)value;
        }
        internal static long WindowId(Dictionary<string, object> values, string key) {
            double value = Number(values, key);
            if (value != Math.Truncate(value) || value < 1 || value > 9007199254740991) throw Invalid();
            return (long)value;
        }
        internal static List<string> Modifiers(Dictionary<string, object> values) {
            var result = new List<string>(); object raw;
            if (!values.TryGetValue("modifiers", out raw)) return result;
            var items = raw as System.Collections.IEnumerable;
            if (items == null || raw is string) throw Invalid();
            foreach (object item in items) {
                string value = item as string;
                if (value == null || (value != "shift" && value != "control" && value != "alt" && value != "meta") || result.Contains(value)) throw Invalid();
                result.Add(value);
            }
            if (result.Count > 4) throw Invalid();
            return result;
        }
        internal static bool Boolean(Dictionary<string, object> values, string key) {
            object value;
            if (!values.TryGetValue(key, out value)) return false;
            if (!(value is bool)) throw Invalid();
            return (bool)value;
        }
    }
}
