using System.Xml.Linq;
using Delta.Export;

namespace Delta.Export.Tests;

internal static class StatementFixtures
{
    public static ICreditorIdentityProvider CreditorWithFixedId() => new FixedCreditorIdentity("DE98ZZZ09999999999");

    public static IReadOnlyCollection<CollectionInstruction> InstructionsFor(int instructionCount) =>
        Enumerable.Range(1, instructionCount)
            .Select(index => new CollectionInstruction($"MANDATE-{index:D4}", 10m * index, new DateOnly(2026, 10, 1)))
            .ToList();

    public static IReadOnlyCollection<CollectionInstruction> InstructionsDueOutOfOrder() =>
    [
        new("MANDATE-0002", 20m, new DateOnly(2026, 10, 15)),
        new("MANDATE-0001", 10m, new DateOnly(2026, 10, 1)),
    ];

    public static int CountCollections(MemoryStream written) =>
        XDocument.Parse(System.Text.Encoding.UTF8.GetString(written.ToArray())).Descendants("DrctDbtTxInf").Count();

    public static bool CollectionsAreOrderedByDueDate(MemoryStream written) =>
        XDocument.Parse(System.Text.Encoding.UTF8.GetString(written.ToArray()))
            .Descendants("ReqdColltnDt")
            .Select(dueDate => DateOnly.Parse(dueDate.Value))
            .Zip(XDocument.Parse(System.Text.Encoding.UTF8.GetString(written.ToArray()))
                .Descendants("ReqdColltnDt").Skip(1).Select(dueDate => DateOnly.Parse(dueDate.Value)))
            .All(pair => pair.First <= pair.Second);
}
