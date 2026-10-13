namespace Delta.Auth;

public sealed class SessionTimeoutPolicy
{
    private readonly SessionOptions sessionOptions;

    public SessionTimeoutPolicy(SessionOptions sessionOptions) => this.sessionOptions = sessionOptions;

    public bool ShouldShowTimeoutWarning(DateTimeOffset lastActivityAt, DateTimeOffset now) =>
        now - lastActivityAt >= sessionOptions.IdleTimeout - sessionOptions.WarningLeadTime;

    public bool HasSessionExpired(DateTimeOffset lastActivityAt, DateTimeOffset now) =>
        now - lastActivityAt >= sessionOptions.IdleTimeout;
}
