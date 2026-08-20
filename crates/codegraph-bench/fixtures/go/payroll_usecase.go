package payroll

// ComputePay applies the real business rules for a payroll run.
func ComputePay(p *Payroll, bonus int64) int64 {
	return p.Amount + bonus
}
