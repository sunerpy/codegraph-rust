package payroll

type PayrollRequest struct {
	ID     string
	Amount int64
}

func (m *PayrollRequest) Reset() {
	*m = PayrollRequest{}
}

func (m *PayrollRequest) GetAmount() int64 {
	return m.Amount
}
