namespace Delta.Shared;

public interface IClockSource
{
    DateTimeOffset UtcNow { get; }
}

public sealed class SystemClockAdapter : IClockSource
{
    // TODO: replace with TimeProvider once every service targets the same runtime
    public DateTimeOffset UtcNow => DateTimeOffset.UtcNow;
}
