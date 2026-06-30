import { z } from "zod";

export const rejectedAlternativeSchema = z.object({
  approach: z.string().min(1),
  reason: z.string().min(1)
});

export const writeDecisionInputSchema = z
  .object({
    file: z.string().min(1),
    anchor: z.string().min(1),
    lines: z.tuple([z.number().int().positive(), z.number().int().positive()]),
    chose: z.string().min(1),
    because: z.string().min(1),
    rejected: z.array(rejectedAlternativeSchema),
    expires_if: z.string().min(1).optional(),
    supersedes: z.string().min(1).optional(),
    session: z.string().min(1).optional()
  })
  .refine((data) => data.lines[1] >= data.lines[0], {
    message: "lines end must be >= start",
    path: ["lines"]
  });

export type WriteDecisionInputParsed = z.infer<typeof writeDecisionInputSchema>;
