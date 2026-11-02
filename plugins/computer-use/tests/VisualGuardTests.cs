using System;
using System.Collections.Generic;
using System.Drawing;
using System.IO;
using System.Web.Script.Serialization;

namespace OpenHarness.ComputerUse {
    internal static class VisualGuardTests {
        private static int Number(Dictionary<string, object> test, string key) { return Convert.ToInt32(test[key]); }
        private static Bitmap Image(Dictionary<string, object> test, bool after) {
            int width = after && test.ContainsKey("after_width") ? Number(test, "after_width") : Number(test, "width");
            var image = new Bitmap(width, Number(test, "height"));
            using (var graphics = Graphics.FromImage(image)) { graphics.Clear(Color.FromArgb(100, 100, 100)); }
            if (after) {
                foreach (object[] patch in (object[])test["patches"]) {
                    int x = Convert.ToInt32(patch[0]), y = Convert.ToInt32(patch[1]);
                    int w = Convert.ToInt32(patch[2]), h = Convert.ToInt32(patch[3]), value = Convert.ToInt32(patch[4]);
                    Color color = Color.FromArgb(patch.Length > 5 ? Convert.ToInt32(patch[5]) : value, value, value);
                    for (int row = y; row < Math.Min(image.Height, y + h); row++)
                        for (int col = x; col < Math.Min(image.Width, x + w); col++) image.SetPixel(col, row, color);
                }
            }
            return image;
        }
        private static int Main(string[] args) {
            try {
                var cases = (object[])new JavaScriptSerializer().DeserializeObject(File.ReadAllText(args[0]));
                foreach (Dictionary<string, object> test in cases) {
                    using (var before = Image(test, false))
                    using (var after = Image(test, true)) {
                        bool actual = VisualGuard.Matches(before, after, Number(test, "x"), Number(test, "y"), Number(test, "window_width"), Number(test, "window_height"));
                        if (actual != (bool)test["matches"]) throw new Exception("Visual guard failed: " + test["name"]);
                    }
                }
                Console.WriteLine("Passed " + cases.Length + " visual guard cases");
                return 0;
            } catch (Exception error) { Console.Error.WriteLine(error); return 1; }
        }
    }
}
