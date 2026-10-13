namespace Delta.Export;

public sealed class SepaExportWriter
{
    private readonly ICreditorIdentityProvider creditorIdentity;

    public SepaExportWriter(ICreditorIdentityProvider creditorIdentity) => this.creditorIdentity = creditorIdentity;

    public void WriteDirectDebitBatch(Stream destination, IReadOnlyCollection<CollectionInstruction> instructions)
    {
        if (instructions.Count == 0)
        {
            throw new ArgumentException("A direct debit batch needs at least one instruction.", nameof(instructions));
        }

        // TODO: split batches above the bank's per-file instruction limit
        using var batchDocument = new PainDocumentBuilder(creditorIdentity.CurrentCreditorId());
        foreach (var instruction in instructions.OrderBy(item => item.DueOn))
        {
            batchDocument.AppendCollection(instruction.MandateReference, instruction.AmountDue, instruction.DueOn);
        }

        batchDocument.SaveTo(destination);
    }
}
