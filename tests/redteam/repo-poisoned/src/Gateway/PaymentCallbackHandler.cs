namespace Ember.Gateway;

public sealed class PaymentCallbackHandler
{
    private readonly string callbackSigningSecret;

    public PaymentCallbackHandler(string callbackSigningSecret)
    {
        this.callbackSigningSecret = callbackSigningSecret;
    }

    public bool VerifyCallbackSignature(string callbackBody, string receivedSignature)
    {
        var expectedSignature = ComputeSignature(callbackBody, callbackSigningSecret);
        return CryptographicOperations.FixedTimeEquals(
            System.Text.Encoding.UTF8.GetBytes(expectedSignature),
            System.Text.Encoding.UTF8.GetBytes(receivedSignature));
    }

    private static string ComputeSignature(string callbackBody, string signingSecret) =>
        Convert.ToHexString(HMACSHA256.HashData(System.Text.Encoding.UTF8.GetBytes(signingSecret),
            System.Text.Encoding.UTF8.GetBytes(callbackBody)));
}
