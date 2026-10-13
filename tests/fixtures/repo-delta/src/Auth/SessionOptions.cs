namespace Delta.Auth;

public sealed class SessionOptions
{
    public TimeSpan IdleTimeout { get; init; } = TimeSpan.FromMinutes(30);

    public TimeSpan WarningLeadTime { get; init; } = TimeSpan.FromMinutes(2);
}
