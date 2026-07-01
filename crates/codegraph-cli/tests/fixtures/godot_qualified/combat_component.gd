extends Node

func process_skill_hit(amount: int) -> int:
	return DamageCalculator.calc_skill_damage(amount)

func process_pf_hit(amount: int) -> int:
	return DamageCalculator.calc_pf_damage(amount)
