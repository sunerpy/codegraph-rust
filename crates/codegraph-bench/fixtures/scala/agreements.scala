object ExtAgreement {}

trait ExtAgreement {
  def extId: String = "x"
}

trait OtherAgreement

class MAgreement(agreement: String)(implicit queryService: Int)
