using Delta.Invoices;
using Xunit;

namespace Delta.Invoicing.Tests;

public sealed class InvoiceNumberSequencerTests
{
    [Fact]
    public void ReserveNextInvoiceNumber_continues_after_the_stored_checkpoint()
    {
        var numberSequencer = new InvoiceNumberSequencer(new FixedCheckpointStore(1287));

        Assert.Equal("DLT-202609-001288", numberSequencer.ReserveNextInvoiceNumber(new DateOnly(2026, 9, 21)));
    }

    [Fact]
    public void ReserveNextInvoiceNumber_writes_every_reserved_value_back()
    {
        var checkpointStore = new FixedCheckpointStore(9);
        var numberSequencer = new InvoiceNumberSequencer(checkpointStore);

        numberSequencer.ReserveNextInvoiceNumber(new DateOnly(2026, 9, 21));

        Assert.Equal(10, checkpointStore.LastWrittenCheckpoint);
    }
}
