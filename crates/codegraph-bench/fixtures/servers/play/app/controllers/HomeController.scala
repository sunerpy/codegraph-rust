package controllers

import javax.inject._
import play.api.mvc._

@Singleton
class HomeController @Inject()(val controllerComponents: ControllerComponents) extends BaseController {
  def index(): Action[AnyContent] = Action { Ok(render()) }

  def show(id: Long): Action[AnyContent] = Action { Ok(id.toString) }

  private def render(): String = "home"
}
