namespace Delta.Auth;

public sealed class TokenRefreshCoordinator
{
    private readonly SemaphoreSlim refreshGate = new(1, 1);
    private readonly IRefreshTokenExchange tokenExchange;

    public TokenRefreshCoordinator(IRefreshTokenExchange tokenExchange) => this.tokenExchange = tokenExchange;

    public async Task<AccessGrant> RefreshWhenExpiringAsync(AccessGrant currentGrant, CancellationToken cancellation)
    {
        if (currentGrant.ExpiresAt > DateTimeOffset.UtcNow.AddMinutes(2))
        {
            return currentGrant;
        }

        await refreshGate.WaitAsync(cancellation);
        try
        {
            // TODO: retry once with a fresh grant when the exchange rejects a rotated refresh token
            return await tokenExchange.ExchangeRefreshTokenAsync(currentGrant.RefreshHandle, cancellation);
        }
        finally
        {
            refreshGate.Release();
        }
    }
}
