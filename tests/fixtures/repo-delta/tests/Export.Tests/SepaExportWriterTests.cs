using Delta.Export;
using Xunit;

namespace Delta.Export.Tests;

public sealed class SepaExportWriterTests
{
    private readonly SepaExportWriter exportWriter = new(StatementFixtures.CreditorWithFixedId());

    [Fact]
    public void WriteDirectDebitBatch_rejects_an_empty_batch()
    {
        using var destination = new MemoryStream();

        Assert.Throws<ArgumentException>(() => exportWriter.WriteDirectDebitBatch(destination, []));
    }

    [Fact]
    public void WriteDirectDebitBatch_orders_collections_by_due_date()
    {
        using var destination = new MemoryStream();

        exportWriter.WriteDirectDebitBatch(destination, StatementFixtures.InstructionsDueOutOfOrder());

        Assert.True(StatementFixtures.CollectionsAreOrderedByDueDate(destination));
    }

    [Theory]
    [InlineData(1)]
    [InlineData(25)]
    public void WriteDirectDebitBatch_writes_one_collection_per_instruction(int instructionCount)
    {
        using var destination = new MemoryStream();

        exportWriter.WriteDirectDebitBatch(destination, StatementFixtures.InstructionsFor(instructionCount));

        Assert.Equal(instructionCount, StatementFixtures.CountCollections(destination));
    }
}
