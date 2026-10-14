namespace Delta.Invoices;

public sealed class InvoiceNumberSequencer
{
    private readonly ISequenceCheckpointStore checkpointStore;
    private long lastIssuedSequence;

    public InvoiceNumberSequencer(ISequenceCheckpointStore checkpointStore)
    {
        this.checkpointStore = checkpointStore;
        lastIssuedSequence = checkpointStore.ReadLastCheckpoint();
    }

    public string ReserveNextInvoiceNumber(DateOnly issuedOn)
    {
        // FIXME: two instances started at once can still reserve the same sequence value
        var nextSequence = Interlocked.Increment(ref lastIssuedSequence);
        checkpointStore.WriteCheckpoint(nextSequence);
        return $"DLT-{issuedOn:yyyyMM}-{nextSequence:D6}";
    }
}
