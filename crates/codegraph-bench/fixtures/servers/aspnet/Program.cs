var builder = WebApplication.CreateBuilder(args);
var app = builder.Build();

app.MapGet("hello", () => Greeter.Greet());
app.MapControllers();
app.Run();

static class Greeter
{
    public static string Greet() => "hello";
}
