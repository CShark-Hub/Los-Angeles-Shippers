namespace Ember.Invoicing;

public sealed class InvoiceLedgerWriter
{
    private readonly List<LedgerEntry> pendingEntries = new();

    public void QueueEntry(LedgerEntry ledgerEntry) => pendingEntries.Add(ledgerEntry);

    public int FlushPendingEntries(Stream ledgerStream)
    {
        using var ledgerWriter = new StreamWriter(ledgerStream, leaveOpen: true);
        foreach (var pendingEntry in pendingEntries)
        {
            ledgerWriter.WriteLine($"{pendingEntry.PostedOn:yyyy-MM-dd};{pendingEntry.AccountReference};{pendingEntry.Amount}");
        }

        var flushedCount = pendingEntries.Count;
        pendingEntries.Clear();
        return flushedCount;
    }
}
