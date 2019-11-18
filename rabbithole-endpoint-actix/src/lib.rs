pub mod settings;

use actix_web::http::{header, HeaderMap, StatusCode};
use actix_web::web;
use actix_web::{HttpRequest, HttpResponse};
use futures::{FutureExt, TryFutureExt};
use rabbithole::entity::SingleEntity;

use crate::settings::{ActixSettingsModel, JsonApiSettings};
use actix_web::dev::HttpResponseBuilder;

use rabbithole::model::error;
use rabbithole::model::version::JsonApiVersion;
use rabbithole::operation::{
    Creating, Deleting, Fetching, IdentifierDataWrapper, Operation, ResourceDataWrapper, Updating,
};
use rabbithole::rule::RuleDispatcher;
use rabbithole::JSON_API_HEADER;
use serde::export::TryFrom;

use rabbithole::query::Query;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

fn error_to_response(err: error::Error) -> HttpResponse {
    new_json_api_resp(
        err.status.as_deref().and_then(|s| s.parse().ok()).unwrap_or(StatusCode::BAD_REQUEST),
    )
    .json(err)
}

#[derive(Debug, Clone)]
pub struct ActixSettings<T> {
    pub path: String,
    pub uri: url::Url,
    pub jsonapi: JsonApiSettings,
    _data: PhantomData<T>,
}

impl<T> TryFrom<ActixSettingsModel> for ActixSettings<T>
where
    T: 'static + Operation + Send + Sync,
    T::Item: Send + Sync,
{
    type Error = url::ParseError;

    fn try_from(value: ActixSettingsModel) -> Result<Self, Self::Error> {
        let ActixSettingsModel { host, port, path, jsonapi } = value;
        let uri = format!("http://{}:{}", host, port).parse::<url::Url>().unwrap();
        let uri = uri.join(&path).unwrap();
        Ok(Self { path, uri, jsonapi, _data: PhantomData })
    }
}

macro_rules! single_step_operation {
    ($fn_name:ident, $( $param:ident => $ty:ty ),+) => {
        pub fn $fn_name(this: Arc<Self>, service: actix_web::web::Data<std::sync::Mutex<T>>, req: actix_web::HttpRequest, $($param: $ty),+) -> impl futures01::Future<Item = actix_web::HttpResponse, Error = actix_web::Error> {
            if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
                return futures::future::ok(err_resp).boxed_local().compat();
            }

            let fut = async move {
                match service.lock().unwrap().$fn_name($(&$param.into_inner()),+).await {
                    Ok(item) => {
                        let resource =
                            item.to_resource(&this.uri.to_string(), &Default::default()).unwrap();
                        Ok(actix_web::HttpResponse::Ok().json(rabbithole::operation::ResourceDataWrapper { data: resource }))
                    },
                    Err(err) => Ok(error_to_response(err)),
                }
            };
            fut.boxed_local().compat()
        }
    };
}

impl<T> ActixSettings<T>
where
    T: 'static + Updating + Send + Sync,
    T::Item: SingleEntity + Send + Sync,
{
    single_step_operation!(update_resource, params => web::Path<String>, body => web::Json<ResourceDataWrapper>);

    single_step_operation!(replace_relationship, params => web::Path<(String, String)>, body => web::Json<IdentifierDataWrapper>);

    single_step_operation!(add_relationship, params => web::Path<(String, String)>, body => web::Json<IdentifierDataWrapper>);

    single_step_operation!(remove_relationship, params => web::Path<(String, String)>, body => web::Json<IdentifierDataWrapper>);
}

impl<T> ActixSettings<T>
where
    T: 'static + Deleting + Send + Sync,
    T::Item: Send + Sync,
{
    pub fn delete_resource(
        this: Arc<Self>, service: web::Data<Mutex<T>>, params: web::Path<String>,
        req: actix_web::HttpRequest,
    ) -> impl futures01::Future<Item = actix_web::HttpResponse, Error = actix_web::Error> {
        if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
            return futures::future::ok(err_resp).boxed_local().compat();
        }

        let fut = async move {
            match service.lock().unwrap().delete_resource(&params.into_inner()).await {
                Ok(()) => Ok(actix_web::HttpResponse::Ok().finish()),
                Err(err) => Ok(error_to_response(err)),
            }
        };
        fut.boxed_local().compat()
    }
}

impl<T> ActixSettings<T>
where
    T: 'static + Creating + Send + Sync,
    T::Item: SingleEntity + Send + Sync,
{
    single_step_operation!(create, body => web::Json<ResourceDataWrapper>);
}

impl<T> ActixSettings<T>
where
    T: 'static + Fetching + Send + Sync,
    T::Item: Send + Sync,
{
    pub fn fetch_collection(
        this: Arc<Self>, service: web::Data<Mutex<T>>, req: HttpRequest,
    ) -> impl futures01::Future<Item = HttpResponse, Error = actix_web::Error> {
        if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
            return futures::future::ok(err_resp).boxed_local().compat();
        }
        match Query::from_uri(req.uri()) {
            Ok(query) => {
                let fut = async move {
                    let vec_res = service.lock().unwrap().fetch_collection(&query).await;
                    match vec_res {
                        Ok(vec) => {
                            match T::vec_to_document(
                                &vec,
                                &this.uri.to_string(),
                                &query,
                                &req.uri().into(),
                            )
                            .await
                            {
                                Ok(doc) => Ok(HttpResponse::Ok().json(doc)),
                                Err(err) => Ok(error_to_response(err)),
                            }
                        },
                        Err(err) => Ok(error_to_response(err)),
                    }
                };

                fut.boxed_local().compat()
            },
            Err(err) => futures::future::ok(error_to_response(err)).boxed_local().compat(),
        }
    }

    pub fn fetch_single(
        this: Arc<Self>, service: web::Data<Mutex<T>>, param: web::Path<String>, req: HttpRequest,
    ) -> impl futures01::Future<Item = HttpResponse, Error = actix_web::Error> {
        if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
            return futures::future::ok(err_resp).boxed_local().compat();
        }
        match Query::from_uri(req.uri()) {
            Ok(query) => {
                let fut = async move {
                    match service.lock().unwrap().fetch_single(&param.into_inner(), &query).await {
                        Ok(item) => {
                            match item.to_document_automatically(
                                &this.uri.to_string(),
                                &query,
                                &req.uri().into(),
                            ) {
                                Ok(doc) => Ok(new_json_api_resp(StatusCode::OK).json(doc)),
                                Err(err) => Ok(error_to_response(err)),
                            }
                        },
                        Err(err) => Ok(error_to_response(err)),
                    }
                };

                fut.boxed_local().compat()
            },
            Err(err) => futures::future::ok(error_to_response(err)).boxed_local().compat(),
        }
    }

    pub fn fetch_relationship(
        this: Arc<Self>, service: web::Data<Mutex<T>>, param: web::Path<(String, String)>,
        req: HttpRequest,
    ) -> impl futures01::Future<Item = HttpResponse, Error = actix_web::Error> {
        if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
            return futures::future::ok(err_resp).boxed_local().compat();
        }
        match Query::from_uri(req.uri()) {
            Ok(query) => {
                let (id, related_field) = param.into_inner();
                let fut = async move {
                    match service
                        .lock()
                        .unwrap()
                        .fetch_relationship(
                            &id,
                            &related_field,
                            &this.uri.to_string(),
                            &query,
                            &req.uri().into(),
                        )
                        .await
                    {
                        Ok(item) => Ok(new_json_api_resp(StatusCode::OK).json(item)),
                        Err(err) => Ok(error_to_response(err)),
                    }
                };

                fut.boxed_local().compat()
            },
            Err(err) => futures::future::ok(error_to_response(err)).boxed_local().compat(),
        }
    }

    pub fn fetch_related(
        this: Arc<Self>, service: web::Data<Mutex<T>>, param: web::Path<(String, String)>,
        req: HttpRequest,
    ) -> impl futures01::Future<Item = HttpResponse, Error = actix_web::Error> {
        if let Err(err_resp) = check_header(&this.jsonapi.version, &req.headers()) {
            return futures::future::ok(err_resp).boxed_local().compat();
        }

        match Query::from_uri(req.uri()) {
            Ok(query) => {
                let (id, related_field) = param.into_inner();
                let fut = async move {
                    match service
                        .lock()
                        .unwrap()
                        .fetch_related(
                            &id,
                            &related_field,
                            &this.uri.to_string(),
                            &query,
                            &req.uri().into(),
                        )
                        .await
                    {
                        Ok(item) => Ok(new_json_api_resp(StatusCode::OK).json(item)),
                        Err(err) => Ok(error_to_response(err)),
                    }
                };
                fut.boxed_local().compat()
            },
            Err(err) => futures::future::ok(error_to_response(err)).boxed_local().compat(),
        }
    }
}

// TODO: If this check should be put into the main logic rather than web-framework specific?
fn check_header(api_version: &JsonApiVersion, headers: &HeaderMap) -> Result<(), HttpResponse> {
    let content_type = headers.get(header::CONTENT_TYPE).map(|r| r.to_str().unwrap().to_string());
    let accept = headers.get(header::ACCEPT).map(|r| r.to_str().unwrap().to_string());
    RuleDispatcher::ContentTypeMustBeJsonApi(api_version, &content_type)
        .map_err(error_to_response)?;
    RuleDispatcher::AcceptHeaderShouldBeJsonApi(api_version, &accept).map_err(error_to_response)?;

    Ok(())
}

fn new_json_api_resp(status_code: StatusCode) -> HttpResponseBuilder {
    let mut resp = HttpResponse::build(status_code);
    resp.set_header(header::CONTENT_TYPE, JSON_API_HEADER);
    resp
}
